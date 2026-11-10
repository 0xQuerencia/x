//! Voice editing, kept apart from general computer control.
//!
//! X remembers a few recent insertions (bounded, in memory only, expiring) so that "scratch
//! that" and "make that shorter" know what "that" is. Remembering is not permission: before
//! any change the target is read back from the app through accessibility and must still hold
//! exactly the expected text. If it cannot be verified, nothing is touched and the person
//! is asked to select the text instead. X never sends Ctrl+Z, and never edits blind.

pub mod commands;
pub mod diff;
pub mod model;
pub mod text;

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::context::a11y::{EditError, FieldState};
use crate::context::{Engine, MAX_SELECTION, WindowRef};
use text::{char_len, slice};

/// Longest insertion X will remember or edit later. Longer ones are simply not tracked.
pub const MAX_TRACKED: usize = 1500;
/// How long an insertion stays editable by "that".
pub const TRACK_TTL: Duration = Duration::from_secs(15 * 60);
const DEPTH: usize = 8;

#[derive(Clone, Debug)]
struct Insertion {
    window: WindowRef,
    text: String,
    at: Instant,
}

/// One change X made, with just enough to reverse it. No document text beyond the
/// edited span is kept.
#[derive(Clone, Debug)]
struct Applied {
    window: WindowRef,
    role: String,
    name: String,
    start: usize,
    old: String,
    new: String,
    count_after: usize,
    at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    Selection,
    Insertion,
}

/// Exactly what an edit will change, and where, as read from the app a moment ago.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub window: WindowRef,
    pub role: String,
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub origin: Origin,
    /// Length of the whole field when read: if it changes, offsets cannot be trusted.
    pub count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditFail {
    NoWindow,
    NothingSelected(String),
    NothingRecent,
    Unreadable(String),
    NotEditable(String),
    TooLong,
    /// The text is no longer what X expected; the person must re-select.
    Changed(String),
    Stale(String),
    NothingToUndo,
    Failed(String),
}

impl std::fmt::Display for EditFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWindow => f.write_str("No window has focus, so there is nothing to edit"),
            Self::NothingSelected(app) => write!(f, "Nothing is selected in {app}. Select the text and ask again."),
            Self::NothingRecent => f.write_str("X has not inserted anything here recently. Select the text you mean and ask again."),
            Self::Unreadable(why) => write!(f, "X cannot read this field to check it ({why}). Nothing was changed."),
            Self::NotEditable(why) => write!(f, "X cannot safely edit this field ({why}). Nothing was changed."),
            Self::TooLong => f.write_str("That text is too long for X to edit safely. Select less."),
            Self::Changed(what) => write!(f, "{what}. Nothing was changed. Select the text you mean and ask again."),
            Self::Stale(why) => write!(f, "{why}. Nothing was changed."),
            Self::NothingToUndo => f.write_str("Nothing X changed can be undone safely. X does not send Ctrl+Z blindly."),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

impl From<EditError> for EditFail {
    fn from(e: EditError) -> Self {
        match e {
            EditError::Changed => Self::Changed("The text changed while X was working".into()),
            EditError::Partial(m) => Self::Failed(format!(
                "{m}. The original text may have been removed; press Undo in the app"
            )),
            EditError::Refused(m) => Self::NotEditable(m),
        }
    }
}

/// What a finished change tells the person.
#[derive(Debug, Eq, PartialEq)]
pub struct Done {
    pub message: String,
}

#[derive(Default)]
struct Memory {
    inserted: VecDeque<Insertion>,
    applied: VecDeque<Applied>,
}

#[derive(Default)]
pub struct Editor {
    memory: Mutex<Memory>,
}

fn push<T>(q: &mut VecDeque<T>, item: T) {
    q.push_back(item);
    while q.len() > DEPTH {
        q.pop_front();
    }
}

impl Editor {
    fn memory(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.memory.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Called after X types or pastes dictation into `window`.
    pub fn record_insertion(&self, window: WindowRef, text: &str) {
        let n = char_len(text);
        if n == 0 || n > MAX_TRACKED {
            return;
        }
        push(
            &mut self.memory().inserted,
            Insertion {
                window,
                text: text.to_string(),
                at: Instant::now(),
            },
        );
    }

    /// Forgets everything: on exit, or when the person asks.
    pub fn forget(&self) {
        let mut m = self.memory();
        m.inserted.clear();
        m.applied.clear();
    }

    fn recent(&self, window: &WindowRef) -> Option<Insertion> {
        self.memory()
            .inserted
            .iter()
            .rev()
            .find(|i| i.window == *window && i.at.elapsed() <= TRACK_TTL)
            .cloned()
    }

    fn focused(engine: &Engine) -> Result<(WindowRef, String), EditFail> {
        let ctx = engine.capture(false);
        let win = ctx.window_ref().cloned().ok_or(EditFail::NoWindow)?;
        Ok((win, ctx.app()))
    }

    fn read(
        engine: &Engine,
        win: &WindowRef,
        window: usize,
        range: Option<(usize, usize)>,
    ) -> Result<FieldState, EditFail> {
        engine
            .a11y()
            .field(win.pid, window, range)
            .map_err(EditFail::Unreadable)
    }

    /// Finds what `scope` means right now and verifies it against the app.
    pub fn resolve(&self, engine: &Engine, scope: commands::Scope) -> Result<Target, EditFail> {
        let (win, app) = Self::focused(engine)?;
        let field = Self::read(engine, &win, MAX_TRACKED + 16, None)?;
        if let Some((start, end)) = field.selection {
            return match field.selected.clone() {
                Some(text) if !text.trim().is_empty() && end - start <= MAX_SELECTION => {
                    Ok(target(win, &field, start, end, text, Origin::Selection))
                }
                _ => Err(EditFail::TooLong),
            };
        }
        if scope == commands::Scope::Selection {
            return Err(EditFail::NothingSelected(app));
        }
        let ins = self.recent(&win).ok_or(EditFail::NothingRecent)?;
        let n = char_len(&ins.text);
        let before = char_len(&field.before);
        let tail =
            (field.caret >= n && before >= n).then(|| slice(&field.before, before - n, before));
        if tail.as_deref() != Some(ins.text.as_str()) {
            return Err(EditFail::Changed(
                "The text before the cursor is no longer what X inserted".into(),
            ));
        }
        Ok(target(
            win,
            &field,
            field.caret - n,
            field.caret,
            ins.text,
            Origin::Insertion,
        ))
    }

    /// The range of `target.text` (relative offsets) turned into a verified target.
    pub fn narrow(target: &Target, rel: (usize, usize)) -> Target {
        Target {
            start: target.start + rel.0,
            end: target.start + rel.1,
            text: slice(&target.text, rel.0, rel.1),
            ..target.clone()
        }
    }

    /// Replaces the target's text with `new`, after focusing its window and re-reading
    /// the field. Refuses unless the text there is exactly what was captured.
    pub fn apply(&self, engine: &Engine, t: &Target, new: &str) -> Result<Done, EditFail> {
        if char_len(new) > MAX_SELECTION * 2 {
            return Err(EditFail::TooLong);
        }
        if Self::focused(engine).ok().map(|f| f.0).as_ref() != Some(&t.window) {
            // The preview itself may have taken focus: put the person back, verified.
            engine.windows().focus(&t.window).map_err(|e| {
                EditFail::Stale(format!("The original window could not be focused ({e})"))
            })?;
            if Self::focused(engine)?.0 != t.window {
                return Err(EditFail::Stale(
                    "Focus is not on the original window".into(),
                ));
            }
        }
        let field = Self::read(engine, &t.window, 0, Some((t.start, t.end)))?;
        if field.role != t.role || field.name != t.name {
            return Err(EditFail::Changed(
                "The focused field is not the one that was edited".into(),
            ));
        }
        if field.count != t.count || field.range_text.as_deref() != Some(t.text.as_str()) {
            return Err(EditFail::Changed(
                "The original text changed while the preview was open".into(),
            ));
        }
        engine
            .a11y()
            .replace(t.window.pid, t.start, t.end, &t.text, new)?;
        let count_after = t.count - char_len(&t.text) + char_len(new);
        self.verify(engine, t, t.start, new, count_after)?;
        let mut m = self.memory();
        // "that" keeps pointing at the text as it now stands.
        if let Some(i) = m
            .inserted
            .iter_mut()
            .rev()
            .find(|i| i.window == t.window && i.text == t.text)
        {
            i.text = new.to_string();
        }
        m.inserted.retain(|i| !i.text.is_empty());
        push(
            &mut m.applied,
            Applied {
                window: t.window.clone(),
                role: t.role.clone(),
                name: t.name.clone(),
                start: t.start,
                old: t.text.clone(),
                new: new.to_string(),
                count_after,
                at: Instant::now(),
            },
        );
        Ok(Done {
            message: "Replaced".into(),
        })
    }

    /// Reads the field back: the new text must be where it was put and the field the
    /// length it should be. Success is what the app reports, not what X asked for.
    fn verify(
        &self,
        engine: &Engine,
        t: &Target,
        start: usize,
        new: &str,
        count: usize,
    ) -> Result<(), EditFail> {
        let end = start + char_len(new);
        let field = Self::read(engine, &t.window, 0, Some((start, end)))?;
        if field.count == count && field.range_text.as_deref() == Some(new) {
            Ok(())
        } else {
            Err(EditFail::Failed(
                "The app accepted the change but the text does not read back as expected. Check it before continuing".into(),
            ))
        }
    }

    /// Inserts at the caret (for "new paragraph"), verified.
    pub fn insert_at_caret(&self, engine: &Engine, text: &str) -> Result<Done, EditFail> {
        let (win, _) = Self::focused(engine)?;
        let field = Self::read(engine, &win, 0, None)?;
        if !field.can_edit {
            return Err(EditFail::NotEditable(
                "it does not accept edits from accessibility".into(),
            ));
        }
        engine.a11y().insert(win.pid, field.caret, text)?;
        let count = field.count + char_len(text);
        let t = Target {
            window: win.clone(),
            role: field.role.clone(),
            name: field.name.clone(),
            start: field.caret,
            end: field.caret,
            text: String::new(),
            origin: Origin::Insertion,
            count: field.count,
        };
        self.verify(engine, &t, field.caret, text, count)?;
        push(
            &mut self.memory().applied,
            Applied {
                window: win,
                role: field.role,
                name: field.name,
                start: field.caret,
                old: String::new(),
                new: text.to_string(),
                count_after: count,
                at: Instant::now(),
            },
        );
        Ok(Done {
            message: "Added a new paragraph".into(),
        })
    }

    /// Selects a verified range, so the person can act on it.
    pub fn select(&self, engine: &Engine, t: &Target) -> Result<Done, EditFail> {
        engine.a11y().select(t.window.pid, t.start, t.end)?;
        let field = Self::read(engine, &t.window, 0, None)?;
        if field.selection == Some((t.start, t.end)) {
            Ok(Done {
                message: "Selected".into(),
            })
        } else {
            Err(EditFail::Failed(
                "The app did not keep the selection".into(),
            ))
        }
    }

    /// Reverses the newest X change, only if the field still holds exactly what that
    /// change left: same window and field, same length, and the changed span unchanged.
    pub fn undo(&self, engine: &Engine) -> Result<Done, EditFail> {
        let (win, _) = Self::focused(engine)?;
        let (last, ins) = {
            let m = self.memory();
            (
                m.applied
                    .back()
                    .filter(|a| a.at.elapsed() <= TRACK_TTL)
                    .cloned(),
                m.inserted
                    .back()
                    .filter(|i| i.at.elapsed() <= TRACK_TTL)
                    .cloned(),
            )
        };
        // An insertion newer than the last edit is what "undo" most likely means.
        if let Some(ins) =
            ins.filter(|i| last.as_ref().is_none_or(|a| i.at > a.at) && i.window == win)
        {
            let t = self.resolve_insertion(engine, &win, &ins)?;
            self.apply(engine, &t, "")?;
            return Ok(Done {
                message: "Removed the last insertion".into(),
            });
        }
        let last = last.ok_or(EditFail::NothingToUndo)?;
        if last.window != win {
            return Err(EditFail::Stale(
                "Focus is not on the window that was edited".into(),
            ));
        }
        let end = last.start + char_len(&last.new);
        let field = Self::read(engine, &win, 0, Some((last.start, end)))?;
        if field.role != last.role
            || field.name != last.name
            || field.count != last.count_after
            || field.range_text.as_deref() != Some(last.new.as_str())
        {
            return Err(EditFail::Changed(
                "The text has changed since X's edit".into(),
            ));
        }
        if last.new.is_empty() {
            engine.a11y().insert(win.pid, last.start, &last.old)?;
        } else {
            engine
                .a11y()
                .replace(win.pid, last.start, end, &last.new, &last.old)?;
        }
        let count = last.count_after - char_len(&last.new) + char_len(&last.old);
        let back = Target {
            window: win,
            role: last.role.clone(),
            name: last.name.clone(),
            start: last.start,
            end: last.start,
            text: String::new(),
            origin: Origin::Insertion,
            count: field.count,
        };
        self.verify(engine, &back, last.start, &last.old, count)?;
        self.memory().applied.pop_back();
        Ok(Done {
            message: "Undid X's last edit".into(),
        })
    }

    fn resolve_insertion(
        &self,
        engine: &Engine,
        win: &WindowRef,
        ins: &Insertion,
    ) -> Result<Target, EditFail> {
        let field = Self::read(engine, win, MAX_TRACKED + 16, None)?;
        let n = char_len(&ins.text);
        let before = char_len(&field.before);
        let tail =
            (field.caret >= n && before >= n).then(|| slice(&field.before, before - n, before));
        if tail.as_deref() != Some(ins.text.as_str()) {
            return Err(EditFail::Changed(
                "The text before the cursor is no longer what X inserted".into(),
            ));
        }
        Ok(target(
            win.clone(),
            &field,
            field.caret - n,
            field.caret,
            ins.text.clone(),
            Origin::Insertion,
        ))
    }

    /// Removes a verified span of the last insertion ("scratch that", "delete the last word").
    pub fn remove(
        &self,
        engine: &Engine,
        whole: &Target,
        rel: (usize, usize),
    ) -> Result<Done, EditFail> {
        let part = Self::narrow(whole, rel);
        let done = self.apply(engine, &part, "")?;
        if whole.origin == Origin::Insertion {
            let kept = slice(&whole.text, 0, rel.0);
            let mut m = self.memory();
            if let Some(i) = m
                .inserted
                .iter_mut()
                .rev()
                .find(|i| i.window == whole.window && i.text == whole.text)
            {
                i.text = kept;
            }
            m.inserted.retain(|i| !i.text.is_empty());
        }
        Ok(done)
    }

    pub fn scratch(&self, engine: &Engine) -> Result<Done, EditFail> {
        let whole = self.resolve(engine, commands::Scope::Recent)?;
        if whole.origin == Origin::Selection {
            return Err(EditFail::Failed(
                "Something is selected. Scratch that only removes what X just typed; delete the selection yourself".into(),
            ));
        }
        self.remove(engine, &whole, (0, char_len(&whole.text)))?;
        Ok(Done {
            message: "Removed the last insertion".into(),
        })
    }
}

fn target(
    window: WindowRef,
    f: &FieldState,
    start: usize,
    end: usize,
    text: String,
    origin: Origin,
) -> Target {
    Target {
        window,
        role: f.role.clone(),
        name: f.name.clone(),
        start,
        end,
        text,
        origin,
        count: f.count,
    }
}

#[cfg(test)]
pub(crate) mod tests;

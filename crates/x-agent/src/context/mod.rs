//! What the person is looking at, read on demand and only when a command needs it.
//!
//! A [`Context`] is a short-lived snapshot: the focused window (from the compositor), the
//! focused control and any text selection (from accessibility), each tagged with where it
//! came from and how far to trust it. Nothing here polls, watches keys or reads the
//! clipboard. Everything read from an app is untrusted data; it can become a search query
//! or a model prompt body, never a command.

pub mod a11y;
pub mod commands;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::wm::{self, Cap, WinInfo, WmError};
use a11y::{Accessibility, Focus};

/// Longest selection X will hold. Longer text is cut and flagged, never silently dropped.
pub const MAX_SELECTION: usize = 8000;
/// A snapshot older than this is not trusted to describe the screen any more.
pub const FRESH_FOR: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    WindowManager,
    Accessibility,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Sourced<T> {
    pub value: T,
    pub source: Source,
    pub confidence: Confidence,
}

/// The identity that must still match before an old window reference is trusted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WindowRef {
    pub id: String,
    pub pid: u32,
    pub class: String,
}

impl From<&WinInfo> for WindowRef {
    fn from(w: &WinInfo) -> Self {
        Self {
            id: w.id.clone(),
            pid: w.pid,
            class: w.class.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WindowCtx {
    pub window: WindowRef,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldCtx {
    pub role: String,
    pub name: String,
    pub editable: bool,
    pub sensitive: bool,
}

/// Why there is no usable selection. Each case reads differently to the person.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Selection {
    Text {
        text: String,
        truncated: bool,
    },
    Empty,
    /// A password or similar field: the text was never read.
    Withheld,
    /// The app does not expose a selection to accessibility (or no helper is installed).
    Unavailable(String),
    /// The selection was not asked for by this command.
    NotRead,
}

#[derive(Clone, Debug)]
pub struct Context {
    pub captured: Instant,
    pub window: Option<Sourced<WindowCtx>>,
    pub workspace: Option<Sourced<i64>>,
    pub previous: Option<WindowRef>,
    pub field: Option<Sourced<FieldCtx>>,
    pub selection: Sourced<Selection>,
    /// Capability gaps and oddities worth showing, such as "Hyprland cannot list...".
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextError {
    NoWindow,
    NoSelection(String),
    Sensitive,
    Unavailable(String),
    Stale(String),
    Unsupported(String),
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWindow => f.write_str("No window has focus, so there is nothing to refer to"),
            Self::NoSelection(app) => write!(
                f,
                "Nothing is selected in {app}. Select the text and ask again."
            ),
            Self::Sensitive => {
                f.write_str("That field looks sensitive, so X did not read it. Nothing was done.")
            }
            Self::Unavailable(why) => write!(f, "X cannot read the selection here: {why}"),
            Self::Stale(why) => write!(f, "{why}. Nothing was done; ask again."),
            Self::Unsupported(what) => f.write_str(what),
        }
    }
}

impl Context {
    pub fn is_fresh(&self) -> bool {
        self.captured.elapsed() <= FRESH_FOR
    }

    pub fn app(&self) -> String {
        self.window.as_ref().map_or_else(
            || "the focused window".into(),
            |w| w.value.window.class.clone(),
        )
    }

    pub fn window_ref(&self) -> Option<&WindowRef> {
        self.window.as_ref().map(|w| &w.value.window)
    }

    /// The selected text, or the precise reason there is none. Never guesses.
    pub fn selected_text(&self) -> Result<&str, ContextError> {
        if self.window.is_none() {
            return Err(ContextError::NoWindow);
        }
        if !self.is_fresh() {
            return Err(ContextError::Stale(
                "The window state is out of date".into(),
            ));
        }
        match &self.selection.value {
            Selection::Text { text, .. } if !text.trim().is_empty() => Ok(text),
            Selection::Text { .. } | Selection::Empty => Err(ContextError::NoSelection(self.app())),
            Selection::Withheld => Err(ContextError::Sensitive),
            Selection::Unavailable(why) => Err(ContextError::Unavailable(why.clone())),
            Selection::NotRead => Err(ContextError::Unavailable("it was not requested".into())),
        }
    }

    pub fn selection_truncated(&self) -> bool {
        matches!(
            &self.selection.value,
            Selection::Text {
                truncated: true,
                ..
            }
        )
    }
}

/// The desktop, as the window manager reports it. Faked in tests.
pub trait Windows: Send + Sync {
    fn view(&self) -> Result<wm::View, WmError>;
}

pub struct SystemWindows;

impl Windows for SystemWindows {
    fn view(&self) -> Result<wm::View, WmError> {
        wm::view()
    }
}

pub struct Engine {
    windows: Arc<dyn Windows>,
    a11y: Arc<dyn Accessibility>,
}

impl Engine {
    pub fn new(windows: Arc<dyn Windows>, a11y: Arc<dyn Accessibility>) -> Self {
        Self { windows, a11y }
    }

    pub fn system() -> Self {
        Self::new(Arc::new(SystemWindows), Arc::new(a11y::Atspi))
    }

    /// One snapshot, taken now. `read_selection` is false for commands that only need to
    /// know which window is focused, so those never touch accessibility at all.
    pub fn capture(&self, read_selection: bool) -> Context {
        let mut notes = Vec::new();
        let view = self.windows.view();
        let (window, workspace, previous) = match &view {
            Ok(v) => (
                v.focused.as_ref().map(window_ctx),
                v.workspace.map(|n| Sourced {
                    value: n,
                    source: Source::WindowManager,
                    confidence: Confidence::High,
                }),
                v.previous.as_ref().map(WindowRef::from),
            ),
            Err(e) => {
                notes.push(format!("Window information is unavailable: {e}"));
                (None, None, None)
            }
        };
        if let Ok(v) = &view {
            if !v.caps.contains(&Cap::Focus) {
                notes.push(format!("{} cannot focus windows", v.backend));
            }
            if v.workspace.is_none() {
                notes.push("The active workspace is not reported on this desktop".into());
            }
        }
        let (field, selection) = match (&window, read_selection) {
            (Some(win), true) => self.read_focus(win.value.window.pid, &mut notes),
            (_, false) => (None, not_read()),
            (None, true) => (None, unavailable("no window has focus")),
        };
        Context {
            captured: Instant::now(),
            window,
            workspace,
            previous,
            field,
            selection,
            notes,
        }
    }

    fn read_focus(
        &self,
        pid: u32,
        notes: &mut Vec<String>,
    ) -> (Option<Sourced<FieldCtx>>, Sourced<Selection>) {
        let focus = match self.a11y.focus(pid) {
            Ok(f) => f,
            Err(why) => {
                notes.push(format!("Accessibility: {why}"));
                return (None, unavailable(&why));
            }
        };
        let sensitive = focus.is_sensitive();
        let field = Sourced {
            value: FieldCtx {
                role: focus.role.clone(),
                name: focus.name.clone(),
                editable: focus.editable,
                sensitive,
            },
            source: Source::Accessibility,
            confidence: Confidence::High,
        };
        let selection = if sensitive {
            Selection::Withheld
        } else {
            selection_of(&focus)
        };
        let confidence = if focus.via_ancestor {
            Confidence::Medium
        } else {
            Confidence::High
        };
        (
            Some(field),
            Sourced {
                value: selection,
                source: Source::Accessibility,
                confidence,
            },
        )
    }

    /// Verifies immediately before an action that the world still matches `ctx`. With
    /// `strict_focus` the same window must still be focused (a command that runs with no
    /// prompt); otherwise it is enough that the window still exists, since an approval
    /// prompt may itself have taken focus.
    pub fn recheck(&self, ctx: &Context, strict_focus: bool) -> Result<(), ContextError> {
        let Some(want) = ctx.window_ref() else {
            return Ok(()); // nothing was referred to, so nothing can have gone stale
        };
        let view = self
            .windows
            .view()
            .map_err(|e| ContextError::Stale(format!("The desktop cannot be read ({e})")))?;
        let exists = view.windows.iter().any(|w| WindowRef::from(w) == *want);
        if !exists {
            return Err(ContextError::Stale(format!("{} was closed", want.class)));
        }
        let focused = view.focused.as_ref().map(WindowRef::from);
        if strict_focus && focused.as_ref() != Some(want) {
            return Err(ContextError::Stale("Focus moved to another window".into()));
        }
        Ok(())
    }

    /// Re-reads the selection and requires it to equal what was captured, for edits that
    /// must never land on text that changed in the meantime.
    pub fn selection_unchanged(&self, ctx: &Context) -> Result<(), ContextError> {
        self.recheck(ctx, true)?;
        let Selection::Text { text, .. } = &ctx.selection.value else {
            return Err(ContextError::NoSelection(ctx.app()));
        };
        let now = self.capture(true);
        match now.selection.value {
            Selection::Text { text: now_text, .. } if now_text == *text => Ok(()),
            Selection::Withheld => Err(ContextError::Sensitive),
            _ => Err(ContextError::Stale("The selected text changed".into())),
        }
    }

    /// What this desktop can and cannot do, for `x agent context`.
    pub fn capabilities(&self) -> Capabilities {
        let (backend, windows, focus, workspace, mut notes) = match self.windows.view() {
            Ok(v) => (
                Some(v.backend.to_string()),
                true,
                v.caps.contains(&Cap::Focus),
                v.workspace.is_some(),
                Vec::new(),
            ),
            Err(e) => (None, false, false, false, vec![e.to_string()]),
        };
        let selection = self.a11y.available();
        if let Err(why) = &selection {
            notes.push(why.clone());
        }
        Capabilities {
            backend,
            focused_window: windows,
            focus_windows: focus,
            workspace,
            selection_text: selection.is_ok(),
            notes,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Capabilities {
    pub backend: Option<String>,
    pub focused_window: bool,
    pub focus_windows: bool,
    pub workspace: bool,
    pub selection_text: bool,
    pub notes: Vec<String>,
}

fn window_ctx(w: &WinInfo) -> Sourced<WindowCtx> {
    Sourced {
        value: WindowCtx {
            window: WindowRef::from(w),
            title: w
                .title
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect(),
        },
        source: Source::WindowManager,
        confidence: Confidence::High,
    }
}

fn not_read() -> Sourced<Selection> {
    Sourced {
        value: Selection::NotRead,
        source: Source::Accessibility,
        confidence: Confidence::Low,
    }
}

fn unavailable(why: &str) -> Sourced<Selection> {
    Sourced {
        value: Selection::Unavailable(why.into()),
        source: Source::Accessibility,
        confidence: Confidence::Low,
    }
}

fn selection_of(focus: &Focus) -> Selection {
    match focus.selection.as_deref() {
        None | Some("") => Selection::Empty,
        Some(text) => Selection::Text {
            text: text.to_string(),
            truncated: focus.truncated,
        },
    }
}

#[cfg(test)]
pub(crate) mod tests;

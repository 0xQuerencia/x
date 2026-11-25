//! Window management behind one typed interface. A [`Backend`] talks to one compositor;
//! the [`Controller`] resolves targets from fresh desktop state, verifies every change and
//! remembers a short, bounded undo history. Window titles are untrusted data and never
//! become commands: backends only ever receive ids that came from their own window list.

pub mod apps;
#[cfg(not(windows))]
mod hypr;
#[cfg(not(windows))]
mod i3;
#[cfg(not(windows))]
mod kde;
pub mod keys;
pub mod parse;
mod report;
mod speech;
mod targets;
pub(crate) use targets::{
    prepare_window, prepare_window_choice, run_exact_scoped, run_with_target_scoped,
};
#[cfg(windows)]
mod win11;
#[cfg(windows)]
mod windows_apps;
#[cfg(not(windows))]
mod x11;

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub use report::{
    CapabilityReport, Doctor, KeyFilePermissions, ProbeEvidence, ProbeSource, Report, SupportState,
    detect, doctor, report,
};
#[cfg(windows)]
pub(crate) fn native_handle(id: &str) -> Option<usize> {
    win11::parse_id(id).map(|(handle, _)| handle)
}

const CHOICE_TTL: Duration = Duration::from_secs(30);
const UNDO_DEPTH: usize = 16;
const UNDO_TTL: Duration = Duration::from_secs(600);
const VERIFY_TRIES: u32 = 12;
const WORKSPACES: std::ops::RangeInclusive<i64> = 1..=10;
pub const MINIMIZED: &str = "special:minimized";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WinInfo {
    pub id: String,
    pub pid: u32,
    pub class: String,
    pub title: String,
    /// 0 when the compositor hides it (minimized or scratchpad).
    pub workspace: i64,
    /// Where it really is, so Undo can put it back exactly there.
    pub place: Place,
    pub minimized: bool,
    pub maximized: bool,
    /// 0 is the window used most recently, larger is older; `u32::MAX` when unknown.
    pub recent: u32,
}

impl WinInfo {
    fn same_identity(&self, other: &Self) -> bool {
        self.id == other.id && self.pid == other.pid && self.class == other.class
    }

    /// Identity that must still match before an old reference is trusted again.
    fn key(&self) -> (String, u32, String) {
        (self.id.clone(), self.pid, self.class.clone())
    }
}

/// A workspace number, or a named special one (`special:name`, validated by the backend).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Place {
    N(i64),
    Special(String),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    List,
    Focus,
    SwitchWorkspace,
    MoveToWorkspace,
    Maximize,
    Minimize,
    Close,
}

/// Every operation a backend may be asked for. Ids are validated by the backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Cmd {
    Focus(String),
    Workspace(i64),
    Move { win: String, to: Place },
    Maximize { win: String, on: bool },
    Minimize { win: String },
    Close(String),
}

impl Cmd {
    fn cap(&self) -> Cap {
        match self {
            Self::Focus(_) => Cap::Focus,
            Self::Workspace(_) => Cap::SwitchWorkspace,
            Self::Move { .. } => Cap::MoveToWorkspace,
            Self::Maximize { .. } => Cap::Maximize,
            Self::Minimize { .. } => Cap::Minimize,
            Self::Close(_) => Cap::Close,
        }
    }
}

pub trait Backend: Send {
    fn name(&self) -> &'static str;
    fn caps(&self) -> Vec<Cap>;
    /// Check window enumeration only if the backend can do so with a bounded read.
    fn probe_window_list(&self) -> Option<Result<(), WmError>> {
        None
    }
    /// A fresh read every call; nothing is cached across calls.
    fn windows(&self) -> Result<Vec<WinInfo>, WmError>;
    fn focused(&self) -> Result<Option<String>, WmError>;
    fn workspace(&self) -> Result<i64, WmError>;
    fn run(&self, cmd: &Cmd) -> Result<(), WmError>;
    /// Restore a minimized window without fabricating a virtual-desktop index.
    /// Compositor backends restore via their existing move semantics.
    fn restore_minimized(&self, win: &WinInfo) -> Result<(), WmError> {
        self.run(&Cmd::Move {
            win: win.id.clone(),
            to: win.place.clone(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    pub id: String,
    pub pid: u32,
    pub class: String,
    pub label: String,
}

struct PendingChoice {
    at: Instant,
    request: Request,
    choices: Vec<Choice>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum WmError {
    Unsupported(String),
    NotFound(String),
    Ambiguous(Vec<Choice>),
    Unverified(String),
    Backend(String),
    NothingToUndo,
    StaleUndo(String),
}

impl fmt::Display for WmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(m) | Self::NotFound(m) | Self::Unverified(m) | Self::Backend(m) => {
                f.write_str(m)
            }
            Self::Ambiguous(c) => {
                let list: Vec<_> = c
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{}: {}", i + 1, c.label))
                    .collect();
                write!(
                    f,
                    "Several windows match. Say the number (or use `hex agent choose N`) within 30 seconds: {}",
                    list.join("; ")
                )
            }
            Self::NothingToUndo => f.write_str("Nothing to undo"),
            Self::StaleUndo(m) => write!(f, "Cannot undo: {m}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Focus,
    Back,
    Move,
    Workspace,
    Maximize,
    Minimize,
    Close,
    List,
    Undo,
    /// Questions about the person's own keyboard shortcuts; the question is in `target.app`.
    Keys,
}

/// A workspace by number, or relative to the current one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ws {
    N(u8),
    Next,
    Prev,
}

/// Which window: the focused one, or an approved app. `pick` is a 1-based choice among
/// several windows of that app.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub pick: Option<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub op: Op,
    #[serde(default)]
    pub target: Target,
    #[serde(default)]
    pub ws: Option<Ws>,
}

/// Native identity recorded by the controller before a close is approved.
#[derive(Clone, Debug)]
pub struct TargetIdentity {
    backend: &'static str,
    window: WinInfo,
    expires: Instant,
}

#[derive(Clone, Debug)]
pub enum WindowTarget {
    Exact(TargetIdentity),
    Choose(Vec<Choice>),
}

/// Compatibility name for callers that previously prepared only close actions.
pub type CloseTarget = WindowTarget;

impl WindowTarget {
    pub(crate) fn label(&self) -> Option<String> {
        match self {
            Self::Exact(target) => Some(label(&target.window)),
            Self::Choose(_) => None,
        }
    }

    pub(crate) fn identity(&self) -> Option<crate::context::WindowRef> {
        match self {
            Self::Exact(target) => Some((&target.window).into()),
            Self::Choose(_) => None,
        }
    }

    pub(crate) fn bound_until(&mut self, expires: Instant) {
        if let Self::Exact(target) = self {
            target.expires = target.expires.min(expires);
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Outcome {
    pub message: String,
    pub undoable: bool,
    pub effect: Effect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    Confirmed,
    WindowObservation,
    KeysObservation,
    Unverified,
}

/// One read of the desktop: what exists, what has focus, where we are.
#[derive(Clone, Debug)]
pub struct View {
    pub windows: Vec<WinInfo>,
    pub focused: Option<WinInfo>,
    pub previous: Option<WinInfo>,
    pub workspace: Option<i64>,
    pub backend: &'static str,
    pub caps: Vec<Cap>,
}

enum Undo {
    Workspace(i64),
    Focus(WinInfo),
    Move(WinInfo),
    Maximize(WinInfo),
    Minimize(WinInfo),
}

impl Undo {
    fn win(&self) -> Option<&WinInfo> {
        match self {
            Self::Workspace(_) => None,
            Self::Focus(w) | Self::Move(w) | Self::Maximize(w) | Self::Minimize(w) => Some(w),
        }
    }
}

pub struct Controller {
    backend: Box<dyn Backend>,
    catalog: fn() -> Vec<apps::Entry>,
    binds: fn() -> Vec<keys::Bind>,
    history: VecDeque<(Instant, Undo)>,
    pending: Option<PendingChoice>,
    seen: Option<WinInfo>,
    previous: Option<WinInfo>,
    operation_scope: Option<targets::OperationScope>,
}

impl Controller {
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self {
            backend,
            catalog: apps::load,
            binds: keys::load,
            history: VecDeque::new(),
            pending: None,
            seen: None,
            previous: None,
            operation_scope: None,
        }
    }

    /// Replaces where installed apps come from, so tests need no real desktop.
    #[cfg(test)]
    fn with_catalog(mut self, catalog: fn() -> Vec<apps::Entry>) -> Self {
        self.catalog = catalog;
        self
    }

    #[cfg(test)]
    fn with_binds(mut self, binds: fn() -> Vec<keys::Bind>) -> Self {
        self.binds = binds;
        self
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// A fresh read of every window, the focused one and the current workspace, for
    /// callers that only need to know what is on screen right now.
    pub fn view(&mut self) -> Result<View, WmError> {
        let all = self.backend.windows()?;
        let focused = self.focused_in(&all)?;
        self.note_focus(focused.as_ref());
        Ok(View {
            workspace: self.backend.workspace().ok(),
            previous: self.previous.clone(),
            windows: all,
            focused,
            backend: self.backend.name(),
            caps: self.backend.caps(),
        })
    }

    /// Focuses exactly this window (same id, pid and class), verified. For callers that
    /// hold a reference from earlier and must put the person back where they were.
    pub fn focus_exact(&mut self, id: &str, pid: u32, class: &str) -> Result<(), WmError> {
        let all = self.backend.windows()?;
        let before = self.focused_in(&all)?;
        let win = all
            .iter()
            .find(|w| w.id == id && w.pid == pid && w.class == class)
            .cloned()
            .ok_or_else(|| WmError::NotFound("The window is gone".into()))?;
        self.focus(&win, before).map(drop)
    }

    fn exact(&mut self, id: &str, pid: u32, class: &str) -> Result<WinInfo, WmError> {
        self.backend
            .windows()?
            .into_iter()
            .find(|w| w.id == id && w.pid == pid && w.class == class)
            .ok_or_else(|| WmError::NotFound("The window is gone".into()))
    }

    /// Moves exactly this window to workspace `ws`, verified.
    pub fn move_exact(&mut self, id: &str, pid: u32, class: &str, ws: i64) -> Result<(), WmError> {
        if !WORKSPACES.contains(&ws) {
            return Err(WmError::NotFound("Workspace must be 1-10".into()));
        }
        let win = self.exact(id, pid, class)?;
        self.move_to(&win, ws).map(drop)
    }

    /// Politely closes exactly this window.
    pub fn close_exact(&mut self, id: &str, pid: u32, class: &str) -> Result<(), WmError> {
        let win = self.exact(id, pid, class)?;
        self.close(&win).map(drop)
    }

    fn prepare_close(&self, target: &Target) -> Result<CloseTarget, WmError> {
        self.require(&Cmd::Close(String::new()))?;
        let all = self.backend.windows()?;
        let focused = self.focused_in(&all)?;
        match self.resolve(target, &all, focused.as_ref(), false) {
            Ok(window) => Ok(CloseTarget::Exact(TargetIdentity {
                backend: self.backend.name(),
                window,
                expires: Instant::now() + CHOICE_TTL,
            })),
            Err(WmError::Ambiguous(choices)) => Ok(CloseTarget::Choose(choices)),
            Err(error) => Err(error),
        }
    }

    pub fn run(&mut self, req: &Request) -> Result<Outcome, WmError> {
        self.run_with_target(req, None)
    }

    pub(crate) fn run_with_target(
        &mut self,
        req: &Request,
        prepared: Option<&CloseTarget>,
    ) -> Result<Outcome, WmError> {
        self.pending = None; // a newer task invalidates every old choice
        // Keep all fallible matching inside the closure: a `?` in a match
        // arm must not return before we record an ambiguity for follow-up.
        let result = (|| {
            self.check_operation_scope()?;
            let all = self.backend.windows()?;
            let focused = self.focused_in(&all)?;
            self.check_operation_scope()?;
            self.note_focus(focused.as_ref());
            match req.op {
                Op::List => Ok(list(&all)),
                Op::Keys => Ok(Outcome {
                    message: keys::answer(&(self.binds)(), &req.target.app),
                    undoable: false,
                    effect: Effect::KeysObservation,
                }),
                Op::Undo => self.undo(&all),
                Op::Workspace => {
                    let ws = self.target_ws(req.ws.as_ref())?;
                    self.switch(ws)
                }
                Op::Back => {
                    let prev = match prepared {
                        Some(target) => {
                            self.prepared_window(target, &all, focused.as_ref(), false)?
                        }
                        None => self.fresh(self.previous.clone(), &all, "No previous window")?,
                    };
                    self.focus(&prev, focused)
                }
                Op::Focus => {
                    let win = match prepared {
                        Some(target) => {
                            self.prepared_window(target, &all, focused.as_ref(), false)?
                        }
                        None => self.resolve(&req.target, &all, focused.as_ref(), true)?,
                    };
                    self.focus(&win, focused)
                }
                Op::Move => {
                    let win = match prepared {
                        Some(target) => {
                            self.prepared_window(target, &all, focused.as_ref(), false)?
                        }
                        None => self.resolve(&req.target, &all, focused.as_ref(), false)?,
                    };
                    let ws = self.target_ws(req.ws.as_ref())?;
                    self.move_to(&win, ws)
                }
                Op::Maximize => {
                    let win = match prepared {
                        Some(target) => {
                            self.prepared_window(target, &all, focused.as_ref(), false)?
                        }
                        None => self.resolve(&req.target, &all, focused.as_ref(), false)?,
                    };
                    self.maximize(&win)
                }
                Op::Minimize => {
                    let win = match prepared {
                        Some(target) => {
                            self.prepared_window(target, &all, focused.as_ref(), false)?
                        }
                        None => self.resolve(&req.target, &all, focused.as_ref(), false)?,
                    };
                    self.minimize(&win)
                }
                Op::Close => {
                    let win = match prepared {
                        Some(target) => self.prepared_window(
                            target,
                            &all,
                            focused.as_ref(),
                            req.target.app.is_empty(),
                        )?,
                        None => self.resolve(&req.target, &all, focused.as_ref(), false)?,
                    };
                    let mut out = self.close(&win)?;
                    out.message.push_str(&keys::hint("close", &(self.binds)()));
                    Ok(out)
                }
            }
        })();
        if let Err(WmError::Ambiguous(ref choices)) = result {
            self.pending = Some(PendingChoice {
                at: Instant::now(),
                request: req.clone(),
                choices: choices.clone(),
            });
        }
        result
    }

    /// Stored options retain original id, pid and class. A subsequent window
    /// opening or reordering cannot change the meaning of "two".
    pub fn choices(&self) -> Vec<Choice> {
        self.pending
            .as_ref()
            .filter(|p| p.at.elapsed() <= CHOICE_TTL)
            .map_or_else(Vec::new, |p| p.choices.clone())
    }

    pub fn choose(&mut self, number: usize) -> Result<Outcome, WmError> {
        let pending = self
            .pending
            .take()
            .ok_or_else(|| WmError::NotFound("There is no pending window choice".into()))?;
        if pending.at.elapsed() > CHOICE_TTL {
            return Err(WmError::NotFound(
                "Window choice expired; repeat the original command".into(),
            ));
        }
        let original = pending
            .choices
            .get(
                number
                    .checked_sub(1)
                    .ok_or_else(|| WmError::NotFound("Choose a number starting at one".into()))?,
            )
            .ok_or_else(|| WmError::NotFound("No window has that number".into()))?;
        // Re-observe *after* the person chooses. Never trust mutable list order.
        let all = self.backend.windows()?;
        let win = all
            .iter()
            .find(|w| w.id == original.id && w.pid == original.pid && w.class == original.class)
            .cloned()
            .ok_or_else(|| WmError::NotFound("The selected window is gone".into()))?;
        match pending.request.op {
            Op::Focus => {
                let focused = self.focused_in(&all)?;
                self.focus(&win, focused)
            }
            Op::Move => {
                let dest = self.target_ws(pending.request.ws.as_ref())?;
                self.move_to(&win, dest)
            }
            Op::Maximize => self.maximize(&win),
            Op::Minimize => self.minimize(&win),
            Op::Close => self.close(&win),
            _ => Err(WmError::Unsupported(
                "This operation cannot have numbered choices".into(),
            )),
        }
    }

    fn focused_in(&self, all: &[WinInfo]) -> Result<Option<WinInfo>, WmError> {
        Ok(self
            .backend
            .focused()?
            .and_then(|id| all.iter().find(|w| w.id == id).cloned()))
    }

    /// "Previous window" is whatever had focus before the current one, as last observed.
    fn note_focus(&mut self, now: Option<&WinInfo>) {
        if let (Some(old), Some(now)) = (&self.seen, now)
            && old.id != now.id
        {
            self.previous = Some(old.clone());
        }
        if let Some(now) = now {
            self.seen = Some(now.clone());
        }
    }

    fn require(&self, cmd: &Cmd) -> Result<(), WmError> {
        if self.backend.caps().contains(&cmd.cap()) {
            Ok(())
        } else {
            Err(WmError::Unsupported(format!(
                "{} cannot {:?} on this desktop",
                self.backend.name(),
                cmd.cap()
            )))
        }
    }

    fn fresh(
        &self,
        old: Option<WinInfo>,
        all: &[WinInfo],
        missing: &str,
    ) -> Result<WinInfo, WmError> {
        let old = old.ok_or_else(|| WmError::NotFound(missing.into()))?;
        all.iter()
            .find(|w| w.key() == old.key())
            .cloned()
            .ok_or_else(|| WmError::NotFound(format!("{} is gone", label(&old))))
    }

    /// `prefer_recent` lets a harmless command (focus) take the most recently used match
    /// instead of asking; anything that changes or closes a window still asks.
    fn resolve(
        &self,
        target: &Target,
        all: &[WinInfo],
        focused: Option<&WinInfo>,
        prefer_recent: bool,
    ) -> Result<WinInfo, WmError> {
        if target.app.is_empty() {
            return focused
                .cloned()
                .ok_or_else(|| WmError::NotFound("No window has focus".into()));
        }
        let mut found = matching(&target.app, all, &(self.catalog)())?;
        found.sort_by(|a, b| (a.workspace, &a.id).cmp(&(b.workspace, &b.id)));
        match (found.len(), target.pick) {
            (0, _) => Err(WmError::NotFound(format!(
                "No open window matches \"{}\"",
                label_text(&target.app)
            ))),
            (1, _) => Ok(found.remove(0)),
            (n, Some(p)) if (1..=n).contains(&usize::from(p)) => {
                Ok(found.remove(usize::from(p) - 1))
            }
            // "Close the terminal" while in a terminal means the one in front.
            (_, None) if !prefer_recent && focused.is_some_and(|f| found.contains(f)) => {
                Ok(focused.cloned().unwrap_or_else(|| found.remove(0)))
            }
            (_, None) if prefer_recent && found.iter().any(|w| w.recent != u32::MAX) => {
                // Not the window already in front: the one used just before it.
                let current = focused.map(|f| f.id.clone());
                found.retain(|w| Some(&w.id) != current.as_ref());
                if found.is_empty() {
                    return Err(WmError::NotFound("That window already has focus".into()));
                }
                found.sort_by_key(|w| w.recent);
                Ok(found.remove(0))
            }
            _ => Err(WmError::Ambiguous(
                found
                    .iter()
                    .map(|w| Choice {
                        id: w.id.clone(),
                        pid: w.pid,
                        class: w.class.clone(),
                        label: label(w),
                    })
                    .collect(),
            )),
        }
    }

    fn target_ws(&self, ws: Option<&Ws>) -> Result<i64, WmError> {
        let n = match ws.ok_or_else(|| WmError::NotFound("Which workspace?".into()))? {
            Ws::N(n) => i64::from(*n),
            Ws::Next => self.backend.workspace()? + 1,
            Ws::Prev => self.backend.workspace()? - 1,
        };
        if WORKSPACES.contains(&n) {
            Ok(n)
        } else {
            Err(WmError::NotFound("Workspace must be 1-10".into()))
        }
    }

    fn push(&mut self, undo: Undo) {
        self.history.push_back((Instant::now(), undo));
        while self.history.len() > UNDO_DEPTH {
            self.history.pop_front();
        }
    }

    /// Runs `cmd`, then polls fresh state until `check` holds. A zero exit status or an
    /// "ok" from the compositor proves nothing about the window.
    fn apply(&self, cmd: &Cmd, what: &str, check: impl Fn(&Self) -> bool) -> Result<(), WmError> {
        self.require(cmd)?;
        self.check_operation_scope()?;
        self.backend.run(cmd)?;
        for _ in 0..VERIFY_TRIES {
            self.check_operation_scope()?;
            if check(self) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        Err(WmError::Unverified(format!("{what} did not take effect")))
    }

    fn window(&self, expected: &WinInfo) -> Result<Option<WinInfo>, WmError> {
        Ok(self
            .backend
            .windows()?
            .into_iter()
            .find(|w| w.same_identity(expected)))
    }

    fn has_focus(&self, expected: &WinInfo) -> bool {
        self.backend
            .focused()
            .is_ok_and(|id| id.as_deref() == Some(expected.id.as_str()))
            && self.window(expected).is_ok_and(|window| window.is_some())
    }

    fn switch(&mut self, ws: i64) -> Result<Outcome, WmError> {
        let from = self.backend.workspace()?;
        if from == ws {
            return Ok(done(format!("Already on workspace {ws}"), false));
        }
        let cmd = Cmd::Workspace(ws);
        self.apply(&cmd, &format!("Workspace {ws}"), |c| {
            c.backend.workspace().is_ok_and(|now| now == ws)
        })?;
        self.push(Undo::Workspace(from));
        Ok(done(format!("Now on workspace {ws}"), true))
    }

    fn focus(&mut self, win: &WinInfo, before: Option<WinInfo>) -> Result<Outcome, WmError> {
        if before.as_ref().is_some_and(|b| b.id == win.id) {
            return Ok(done(format!("{} already has focus", label(win)), false));
        }
        if win.minimized {
            self.require(&Cmd::Focus(win.id.clone()))?;
            let mut restore = win.clone();
            if let Ok(ws) = self.backend.workspace() {
                restore.place = Place::N(ws);
            }
            self.check_operation_scope()?;
            self.backend.restore_minimized(&restore)?;
            let mut restored = false;
            for _ in 0..VERIFY_TRIES {
                self.check_operation_scope()?;
                if self
                    .window(win)
                    .is_ok_and(|w| w.is_some_and(|w| !w.minimized))
                {
                    restored = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(60));
            }
            if !restored {
                return Err(WmError::Unverified(
                    "Restoring the window did not take effect".into(),
                ));
            }
        }
        self.apply(&Cmd::Focus(win.id.clone()), "Focus", |c| c.has_focus(win))?;
        if let Some(before) = before {
            self.push(Undo::Focus(before));
        }
        Ok(done(format!("Focused {}", label(win)), true))
    }

    fn move_to(&mut self, win: &WinInfo, ws: i64) -> Result<Outcome, WmError> {
        if win.workspace == ws {
            return Ok(done(
                format!("{} is already on workspace {ws}", label(win)),
                false,
            ));
        }
        let id = win.id.clone();
        self.apply(
            &Cmd::Move {
                win: id.clone(),
                to: Place::N(ws),
            },
            "The move",
            |c| {
                c.window(win)
                    .is_ok_and(|w| w.is_some_and(|w| w.workspace == ws && !w.minimized))
            },
        )?;
        self.push(Undo::Move(win.clone()));
        Ok(done(
            format!("Moved {} to workspace {ws}", label(win)),
            true,
        ))
    }

    fn maximize(&mut self, win: &WinInfo) -> Result<Outcome, WmError> {
        if win.maximized {
            return Ok(done(format!("{} is already maximized", label(win)), false));
        }
        let id = win.id.clone();
        self.apply(
            &Cmd::Maximize {
                win: id.clone(),
                on: true,
            },
            "Maximize",
            |c| c.window(win).is_ok_and(|w| w.is_some_and(|w| w.maximized)),
        )?;
        self.push(Undo::Maximize(win.clone()));
        Ok(done(format!("Maximized {}", label(win)), true))
    }

    fn minimize(&mut self, win: &WinInfo) -> Result<Outcome, WmError> {
        let id = win.id.clone();
        self.apply(&Cmd::Minimize { win: id.clone() }, "Minimize", |c| {
            c.window(win).is_ok_and(|w| w.is_some_and(|w| w.minimized))
        })?;
        self.push(Undo::Minimize(win.clone()));
        Ok(done(format!("Minimized {}", label(win)), true))
    }

    /// Asks politely. Apps with unsaved work may keep the window open, and that is reported
    /// as it is, never forced.
    fn close(&mut self, win: &WinInfo) -> Result<Outcome, WmError> {
        let id = win.id.clone();
        self.require(&Cmd::Close(id.clone()))?;
        self.check_operation_scope()?;
        self.backend.run(&Cmd::Close(id.clone()))?;
        for _ in 0..VERIFY_TRIES {
            self.check_operation_scope()?;
            if self.window(win)?.is_none() {
                return Ok(done(format!("Closed {}", label(win)), false));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(Outcome {
            message: format!(
                "Asked {} to close; it is still open and may be asking to save",
                label(win)
            ),
            undoable: false,
            effect: Effect::Unverified,
        })
    }

    fn undo(&mut self, all: &[WinInfo]) -> Result<Outcome, WmError> {
        let (at, undo) = self.history.pop_back().ok_or(WmError::NothingToUndo)?;
        if at.elapsed() > UNDO_TTL {
            self.history.clear();
            return Err(WmError::NothingToUndo);
        }
        let now = match undo.win() {
            Some(old) => Some(
                all.iter()
                    .find(|w| w.key() == old.key())
                    .cloned()
                    .ok_or_else(|| WmError::StaleUndo(format!("{} is gone", label(old))))?,
            ),
            None => None,
        };
        self.revert(undo, now)
    }

    fn revert(&mut self, undo: Undo, now: Option<WinInfo>) -> Result<Outcome, WmError> {
        let outcome = match (undo, now) {
            (Undo::Workspace(ws), _) => self.switch_quiet(ws)?,
            (Undo::Focus(old), Some(_)) => {
                self.apply(&Cmd::Focus(old.id.clone()), "Focus", |c| c.has_focus(&old))?;
                format!("Focus back on {}", label(&old))
            }
            (Undo::Move(old), Some(_)) => {
                let (id, to) = (old.id.clone(), old.place.clone());
                self.apply(
                    &Cmd::Move {
                        win: id.clone(),
                        to: to.clone(),
                    },
                    "The move back",
                    |c| {
                        c.window(&old)
                            .is_ok_and(|w| w.is_some_and(|w| w.place == to))
                    },
                )?;
                format!("Moved {} back", label(&old))
            }
            (Undo::Maximize(old), Some(_)) => {
                let id = old.id.clone();
                self.apply(
                    &Cmd::Maximize {
                        win: id.clone(),
                        on: false,
                    },
                    "Restore",
                    |c| {
                        c.window(&old)
                            .is_ok_and(|w| w.is_some_and(|w| !w.maximized))
                    },
                )?;
                format!("Restored {}", label(&old))
            }
            (Undo::Minimize(old), Some(_)) => {
                self.backend.restore_minimized(&old)?;
                let mut restored = false;
                for _ in 0..VERIFY_TRIES {
                    if self
                        .window(&old)
                        .is_ok_and(|w| w.is_some_and(|w| !w.minimized && w.place == old.place))
                    {
                        restored = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(60));
                }
                if !restored {
                    return Err(WmError::Unverified("Restore did not take effect".into()));
                }
                format!("Restored {}", label(&old))
            }
            _ => return Err(WmError::StaleUndo("the window changed".into())),
        };
        Ok(done(outcome, false))
    }

    fn switch_quiet(&mut self, ws: i64) -> Result<String, WmError> {
        self.apply(&Cmd::Workspace(ws), &format!("Workspace {ws}"), |c| {
            c.backend.workspace().is_ok_and(|now| now == ws)
        })?;
        Ok(format!("Back on workspace {ws}"))
    }
}

fn done(message: String, undoable: bool) -> Outcome {
    Outcome {
        message,
        undoable,
        effect: Effect::Confirmed,
    }
}

/// App name plus a clipped, control-free title: safe to show, never parsed.
/// Words a person said, safe to show: no control characters, clipped.
fn label_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(40).collect()
}

pub fn label(w: &WinInfo) -> String {
    let title: String = w
        .title
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    if title.is_empty() {
        w.class.clone()
    } else {
        format!("{} ({title})", w.class)
    }
}

fn list(all: &[WinInfo]) -> Outcome {
    let mut lines: Vec<_> = all
        .iter()
        .map(|w| format!("{}: {}", w.workspace, label(w)))
        .collect();
    lines.sort();
    Outcome {
        message: lines.join("; "),
        undoable: false,
        effect: Effect::WindowObservation,
    }
}

/// Windows whose class belongs to what `app` names: an installed app, a role such as
/// "browser", or a class that is open right now.
pub(crate) fn matching(
    app: &str,
    all: &[WinInfo],
    entries: &[apps::Entry],
) -> Result<Vec<WinInfo>, WmError> {
    let classes = apps::evidence(app, entries);
    Ok(all
        .iter()
        .filter(|w| apps::class_matches(&w.class, app, &classes))
        .cloned()
        .collect())
}

static CONTROLLER: Mutex<Option<Controller>> = Mutex::new(None);

/// Runs `f` on the session's controller, so context and undo outlive single commands.
fn with_controller<T>(f: impl FnOnce(&mut Controller) -> Result<T, WmError>) -> Result<T, WmError> {
    let mut slot = CONTROLLER
        .lock()
        .map_err(|_| WmError::Backend("window state is poisoned".into()))?;
    if slot.is_none() {
        let backend = detect().map_err(WmError::Unsupported)?;
        *slot = Some(Controller::new(backend));
    }
    let out = slot.as_mut().map_or(Err(WmError::NothingToUndo), f);
    if matches!(out, Err(WmError::Backend(_))) {
        *slot = None; // a crashed or disconnected backend is rebuilt on the next command
    }
    out
}

pub fn run(req: &Request) -> Result<Outcome, WmError> {
    run_with_target(req, None)
}

pub(crate) fn prepare_close(target: &Target) -> Result<CloseTarget, WmError> {
    with_controller(|c| c.prepare_close(target))
}

pub(crate) fn run_with_target(
    request: &Request,
    target: Option<&CloseTarget>,
) -> Result<Outcome, WmError> {
    with_controller(|c| c.run_with_target(request, target))
}

/// Whether there is an Undo entry created in the past eight seconds.
/// Unlike `with_controller`, this read never initializes a backend, touches a
/// real window, or runs a compositor command from the UI thread.
pub fn latest_undo_at() -> Option<Instant> {
    CONTROLLER.lock().ok().and_then(|slot| {
        slot.as_ref()
            .and_then(|controller| controller.history.back())
            .map(|(at, _)| *at)
    })
}

pub fn recent_undo_available() -> bool {
    latest_undo_at().is_some_and(|at| at.elapsed() <= Duration::from_secs(8))
}

pub fn choices() -> Result<Vec<Choice>, WmError> {
    with_controller(|c| Ok(c.choices()))
}

pub fn choose(index: usize) -> Result<Outcome, WmError> {
    with_controller(|c| c.choose(index))
}

pub fn focus_exact(id: &str, pid: u32, class: &str) -> Result<(), WmError> {
    with_controller(|c| c.focus_exact(id, pid, class))
}

pub fn move_exact(id: &str, pid: u32, class: &str, ws: i64) -> Result<(), WmError> {
    with_controller(|c| c.move_exact(id, pid, class, ws))
}

pub fn close_exact(id: &str, pid: u32, class: &str) -> Result<(), WmError> {
    with_controller(|c| c.close_exact(id, pid, class))
}

/// The desktop as it is right now (one fresh read, nothing cached).
pub fn view() -> Result<View, WmError> {
    with_controller(Controller::view)
}

#[cfg(test)]
mod tests;

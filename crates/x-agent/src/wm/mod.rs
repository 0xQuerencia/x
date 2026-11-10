//! Window management behind one typed interface. A [`Backend`] talks to one compositor;
//! the [`Controller`] resolves targets from fresh desktop state, verifies every change and
//! remembers a short, bounded undo history. Window titles are untrusted data and never
//! become commands: backends only ever receive ids that came from their own window list.

mod hypr;
mod i3;
pub mod parse;
mod report;

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub use report::{Report, detect, report};

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
}

impl WinInfo {
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
    /// A fresh read every call; nothing is cached across calls.
    fn windows(&self) -> Result<Vec<WinInfo>, WmError>;
    fn focused(&self) -> Result<Option<String>, WmError>;
    fn workspace(&self) -> Result<i64, WmError>;
    fn run(&self, cmd: &Cmd) -> Result<(), WmError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    pub id: String,
    pub label: String,
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
                    "Several windows match, say a number: {}",
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

#[derive(Debug, Eq, PartialEq)]
pub struct Outcome {
    pub message: String,
    pub undoable: bool,
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
    history: VecDeque<(Instant, Undo)>,
    seen: Option<WinInfo>,
    previous: Option<WinInfo>,
}

impl Controller {
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self {
            backend,
            history: VecDeque::new(),
            seen: None,
            previous: None,
        }
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

    pub fn run(&mut self, req: &Request) -> Result<Outcome, WmError> {
        let all = self.backend.windows()?;
        let focused = self.focused_in(&all)?;
        self.note_focus(focused.as_ref());
        match req.op {
            Op::List => Ok(list(&all)),
            Op::Undo => self.undo(&all),
            Op::Workspace => {
                let ws = self.target_ws(req.ws.as_ref())?;
                self.switch(ws)
            }
            Op::Back => {
                let prev = self.fresh(self.previous.clone(), &all, "No previous window")?;
                self.focus(&prev, focused)
            }
            Op::Focus => {
                let win = self.resolve(&req.target, &all, focused.as_ref())?;
                self.focus(&win, focused)
            }
            Op::Move => {
                let win = self.resolve(&req.target, &all, focused.as_ref())?;
                let ws = self.target_ws(req.ws.as_ref())?;
                self.move_to(&win, ws)
            }
            Op::Maximize => {
                let win = self.resolve(&req.target, &all, focused.as_ref())?;
                self.maximize(&win)
            }
            Op::Minimize => {
                let win = self.resolve(&req.target, &all, focused.as_ref())?;
                self.minimize(&win)
            }
            Op::Close => {
                let win = self.resolve(&req.target, &all, focused.as_ref())?;
                self.close(&win)
            }
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

    fn resolve(
        &self,
        target: &Target,
        all: &[WinInfo],
        focused: Option<&WinInfo>,
    ) -> Result<WinInfo, WmError> {
        if target.app.is_empty() {
            return focused
                .cloned()
                .ok_or_else(|| WmError::NotFound("No window has focus".into()));
        }
        let mut found = matching(&target.app, all)?;
        found.sort_by(|a, b| (a.workspace, &a.id).cmp(&(b.workspace, &b.id)));
        match (found.len(), target.pick) {
            (0, _) => Err(WmError::NotFound(format!("No open {} window", target.app))),
            (1, _) => Ok(found.remove(0)),
            (n, Some(p)) if (1..=n).contains(&usize::from(p)) => {
                Ok(found.remove(usize::from(p) - 1))
            }
            _ => Err(WmError::Ambiguous(
                found
                    .iter()
                    .map(|w| Choice {
                        id: w.id.clone(),
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
        self.backend.run(cmd)?;
        for _ in 0..VERIFY_TRIES {
            if check(self) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        Err(WmError::Unverified(format!("{what} did not take effect")))
    }

    fn window(&self, id: &str) -> Option<WinInfo> {
        self.backend
            .windows()
            .ok()?
            .into_iter()
            .find(|w| w.id == id)
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
            let ws = self.backend.workspace()?;
            self.apply(
                &Cmd::Move {
                    win: win.id.clone(),
                    to: Place::N(ws),
                },
                "Restoring the window",
                |c| c.window(&win.id).is_some_and(|w| !w.minimized),
            )?;
        }
        let id = win.id.clone();
        self.apply(&Cmd::Focus(id.clone()), "Focus", |c| {
            c.backend.focused().ok().flatten().as_deref() == Some(id.as_str())
        })?;
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
                c.window(&id)
                    .is_some_and(|w| w.workspace == ws && !w.minimized)
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
            |c| c.window(&id).is_some_and(|w| w.maximized),
        )?;
        self.push(Undo::Maximize(win.clone()));
        Ok(done(format!("Maximized {}", label(win)), true))
    }

    fn minimize(&mut self, win: &WinInfo) -> Result<Outcome, WmError> {
        let id = win.id.clone();
        self.apply(&Cmd::Minimize { win: id.clone() }, "Minimize", |c| {
            c.window(&id).is_some_and(|w| w.minimized)
        })?;
        self.push(Undo::Minimize(win.clone()));
        Ok(done(format!("Minimized {}", label(win)), true))
    }

    /// Asks politely. Apps with unsaved work may keep the window open, and that is reported
    /// as it is, never forced.
    fn close(&mut self, win: &WinInfo) -> Result<Outcome, WmError> {
        let id = win.id.clone();
        self.require(&Cmd::Close(id.clone()))?;
        self.backend.run(&Cmd::Close(id.clone()))?;
        for _ in 0..VERIFY_TRIES {
            if self.window(&id).is_none() {
                return Ok(done(format!("Closed {}", label(win)), false));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(done(
            format!(
                "Asked {} to close; it is still open and may be asking to save",
                label(win)
            ),
            false,
        ))
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
                let id = old.id.clone();
                self.apply(&Cmd::Focus(id.clone()), "Focus", |c| {
                    c.backend.focused().ok().flatten().as_deref() == Some(id.as_str())
                })?;
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
                    |c| c.window(&id).is_some_and(|w| w.place == to),
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
                    |c| c.window(&id).is_some_and(|w| !w.maximized),
                )?;
                format!("Restored {}", label(&old))
            }
            (Undo::Minimize(old), Some(_)) => {
                let (id, to) = (old.id.clone(), old.place.clone());
                self.apply(
                    &Cmd::Move {
                        win: id.clone(),
                        to: to.clone(),
                    },
                    "Restore",
                    |c| c.window(&id).is_some_and(|w| !w.minimized && w.place == to),
                )?;
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
    Outcome { message, undoable }
}

/// App name plus a clipped, control-free title: safe to show, never parsed.
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
    done(lines.join("; "), false)
}

/// Windows whose class belongs to an approved app alias.
pub(crate) fn matching(app: &str, all: &[WinInfo]) -> Result<Vec<WinInfo>, WmError> {
    let names = crate::tools::app_classes(app)
        .ok_or_else(|| WmError::NotFound(format!("{app} is not an approved app")))?;
    Ok(all
        .iter()
        .filter(|w| {
            let class = w.class.to_ascii_lowercase();
            names
                .iter()
                .any(|n| class.contains(&n.to_ascii_lowercase()))
        })
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
    with_controller(|c| c.run(req))
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

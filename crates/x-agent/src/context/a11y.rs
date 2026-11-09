//! Reading the focused control and its selection through AT-SPI. The bus is spoken to by a
//! short-lived read-only helper (`atspi_focus.py`) so X needs no extra native dependency;
//! its answer is parsed strictly and treated as untrusted text.

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use super::MAX_SELECTION;
use crate::desktop::{capture, mentions};

const SCRIPT: &str = include_str!("atspi_focus.py");
const HELPER_TIMEOUT: Duration = Duration::from_secs(4);
const SECRET_WORDS: &[&str] = &[
    "password",
    "passcode",
    "passphrase",
    "otp",
    "cvv",
    "cvc",
    "card number",
    "secret",
    "token",
    "api key",
    "2fa",
    "verification code",
];

/// The focused control of one app and what is selected in it.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Focus {
    pub role: String,
    pub name: String,
    pub editable: bool,
    pub protected: bool,
    pub selection: Option<String>,
    pub truncated: bool,
    pub via_ancestor: bool,
}

impl Focus {
    /// Password-type controls, and controls named like a secret, are never read.
    pub fn is_sensitive(&self) -> bool {
        self.protected || self.role.contains("password") || mentions(&self.name, SECRET_WORDS)
    }
}

/// The editing state of the focused text control, read now. Offsets count characters.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FieldState {
    pub role: String,
    pub name: String,
    pub editable: bool,
    pub count: usize,
    pub caret: usize,
    pub selection: Option<(usize, usize)>,
    pub selected: Option<String>,
    /// Up to a window of text ending at the caret, starting at `before_start`.
    pub before: String,
    pub before_start: usize,
    pub after: String,
    /// The control accepts edits through accessibility (no keystrokes needed).
    pub can_edit: bool,
    /// The text of the range asked for, when it was inside the field.
    pub range_text: Option<String>,
}

/// Why an edit through accessibility did not happen (or did only in part).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditError {
    /// The text at the target no longer matches, so nothing was changed.
    Changed,
    /// The original was removed but the replacement was refused.
    Partial(String),
    Refused(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changed => {
                f.write_str("The text changed since it was read, so nothing was changed")
            }
            Self::Partial(m) | Self::Refused(m) => f.write_str(m),
        }
    }
}

pub trait Accessibility: Send + Sync {
    /// The focused control of the app owning `pid`, read now.
    fn focus(&self, pid: u32) -> Result<Focus, String>;
    /// Whether selection reading can work at all on this machine.
    fn available(&self) -> Result<(), String>;
    /// `window` characters either side of the caret, plus the text of `range` if given.
    fn field(
        &self,
        _pid: u32,
        _window: usize,
        _range: Option<(usize, usize)>,
    ) -> Result<FieldState, String> {
        Err("reading fields is not supported here".into())
    }
    /// Replaces `start..end` with `text`, but only if that range still holds `expect`.
    fn replace(
        &self,
        _pid: u32,
        _start: usize,
        _end: usize,
        _expect: &str,
        _text: &str,
    ) -> Result<(), EditError> {
        Err(EditError::Refused("editing is not supported here".into()))
    }
    fn insert(&self, _pid: u32, _at: usize, _text: &str) -> Result<(), EditError> {
        Err(EditError::Refused("editing is not supported here".into()))
    }
    fn select(&self, _pid: u32, _start: usize, _end: usize) -> Result<(), EditError> {
        Err(EditError::Refused("selecting is not supported here".into()))
    }
}

pub struct Atspi;

/// `wl-paste --primary`: what was highlighted last. Read only when asked, never kept.
pub struct WlPrimary;

impl super::PrimarySelection for WlPrimary {
    fn read(&self) -> Result<Option<String>, String> {
        let paste = crate::tools::which("wl-paste").ok_or("wl-paste is not installed")?;
        let mut cmd = Command::new(paste);
        cmd.args(["--primary", "--no-newline", "--type", "text/plain"]);
        match capture(&mut cmd, Duration::from_secs(2)) {
            Ok((true, bytes)) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            // "Nothing is copied" is an exit failure, and means there is no selection.
            Ok((false, _)) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Raw {
    error: Option<String>,
    probe: Option<bool>,
    role: String,
    name: String,
    editable: bool,
    protected: bool,
    selection: Option<String>,
    truncated: bool,
    via_ancestor: bool,
}

/// Parses one helper reply. Anything malformed is an error, never a partial guess.
pub fn parse_reply(bytes: &[u8]) -> Result<Option<Focus>, String> {
    let raw: Raw = serde_json::from_slice(bytes)
        .map_err(|_| "the accessibility helper gave no usable answer".to_string())?;
    if let Some(error) = raw.error {
        return Err(error
            .chars()
            .filter(|c| !c.is_control())
            .take(160)
            .collect());
    }
    if raw.probe == Some(true) {
        return Ok(None);
    }
    let mut focus = Focus {
        role: raw.role,
        name: raw.name,
        editable: raw.editable,
        protected: raw.protected,
        selection: raw.selection,
        truncated: raw.truncated,
        via_ancestor: raw.via_ancestor,
    };
    if focus.is_sensitive() {
        focus.selection = None;
    }
    focus.selection = focus.selection.map(|t| {
        t.chars()
            .filter(|c| *c != '\0')
            .take(MAX_SELECTION)
            .collect()
    });
    Ok(Some(focus))
}

fn run_helper(pid: u32) -> Result<Vec<u8>, String> {
    run_op(pid, "selection", &serde_json::json!({}))
}

fn run_op(pid: u32, op: &str, params: &serde_json::Value) -> Result<Vec<u8>, String> {
    let python = crate::tools::which("python3").ok_or("python3 is not installed")?;
    let mut cmd = Command::new(python);
    cmd.args([
        "-I",
        "-c",
        SCRIPT,
        &pid.to_string(),
        &MAX_SELECTION.to_string(),
    ]);
    cmd.args([op, &params.to_string()]);
    let (ok, out) = capture(&mut cmd, HELPER_TIMEOUT)?;
    if !ok && out.is_empty() {
        return Err("the accessibility helper failed".into());
    }
    Ok(out)
}

impl Accessibility for Atspi {
    fn focus(&self, pid: u32) -> Result<Focus, String> {
        parse_reply(&run_helper(pid.max(1))?)?
            .ok_or_else(|| "the accessibility helper did not report a control".into())
    }

    fn available(&self) -> Result<(), String> {
        parse_reply(&run_helper(0)?).map(drop)
    }

    fn field(
        &self,
        pid: u32,
        window: usize,
        range: Option<(usize, usize)>,
    ) -> Result<FieldState, String> {
        let mut params = serde_json::json!({"window": window});
        if let Some((a, b)) = range {
            params["range"] = serde_json::json!([a, b]);
        }
        parse_field(&run_op(pid.max(1), "field", &params)?)
    }

    fn replace(
        &self,
        pid: u32,
        start: usize,
        end: usize,
        expect: &str,
        text: &str,
    ) -> Result<(), EditError> {
        self.mutate(
            pid,
            "replace",
            serde_json::json!({"start": start, "end": end, "expect": expect, "text": text}),
        )
    }

    fn insert(&self, pid: u32, at: usize, text: &str) -> Result<(), EditError> {
        self.mutate(pid, "insert", serde_json::json!({"at": at, "text": text}))
    }

    fn select(&self, pid: u32, start: usize, end: usize) -> Result<(), EditError> {
        self.mutate(
            pid,
            "select",
            serde_json::json!({"start": start, "end": end}),
        )
    }
}

fn number(v: &serde_json::Value, key: &str) -> Option<usize> {
    usize::try_from(v.get(key)?.as_u64()?).ok()
}

fn text_of(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .chars()
        .filter(|c| *c != '\0')
        .collect()
}

fn reply_value(bytes: &[u8]) -> Result<serde_json::Value, String> {
    serde_json::from_slice(bytes)
        .map_err(|_| "the accessibility helper gave no usable answer".to_string())
}

fn error_of(v: &serde_json::Value) -> Option<String> {
    v.get("error")
        .and_then(serde_json::Value::as_str)
        .map(|e| e.chars().filter(|c| !c.is_control()).take(160).collect())
}

/// Parses a `field` reply. Password fields never carry text, whatever the helper sent.
pub fn parse_field(bytes: &[u8]) -> Result<FieldState, String> {
    let v = reply_value(bytes)?;
    if let Some(error) = error_of(&v) {
        return Err(error);
    }
    let name = text_of(&v, "name");
    let role = text_of(&v, "role");
    let protected = v.get("protected").and_then(serde_json::Value::as_bool) == Some(true);
    if protected || role.contains("password") || mentions(&name, SECRET_WORDS) {
        return Err("that field looks sensitive, so it was not read".into());
    }
    let sel = number(&v, "sel_start").zip(number(&v, "sel_end"));
    Ok(FieldState {
        role,
        name,
        editable: v.get("editable").and_then(serde_json::Value::as_bool) == Some(true),
        count: number(&v, "count").ok_or("the field did not report its length")?,
        caret: number(&v, "caret").ok_or("the field did not report a caret")?,
        selection: sel,
        selected: v
            .get("selected")
            .and_then(serde_json::Value::as_str)
            .map(|t| t.chars().filter(|c| *c != '\0').collect()),
        before: text_of(&v, "before"),
        before_start: number(&v, "before_start").unwrap_or(0),
        after: text_of(&v, "after"),
        can_edit: v.get("can_edit").and_then(serde_json::Value::as_bool) == Some(true),
        range_text: v
            .get("range_text")
            .and_then(serde_json::Value::as_str)
            .map(|t| t.chars().filter(|c| *c != '\0').collect()),
    })
}

/// Parses the reply of a mutating op.
pub fn parse_done(bytes: &[u8]) -> Result<(), EditError> {
    let v = reply_value(bytes).map_err(EditError::Refused)?;
    if let Some(error) = error_of(&v) {
        return Err(
            if v.get("changed").and_then(serde_json::Value::as_bool) == Some(true) {
                EditError::Changed
            } else if v.get("partial").and_then(serde_json::Value::as_bool) == Some(true) {
                EditError::Partial(error)
            } else {
                EditError::Refused(error)
            },
        );
    }
    if v.get("done").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(EditError::Refused(
            "the helper did not confirm the edit".into(),
        ))
    }
}

impl Atspi {
    fn mutate(&self, pid: u32, op: &str, params: serde_json::Value) -> Result<(), EditError> {
        parse_done(&run_op(pid.max(1), op, &params).map_err(EditError::Refused)?)
    }
}

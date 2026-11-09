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

pub trait Accessibility: Send + Sync {
    /// The focused control of the app owning `pid`, read now.
    fn focus(&self, pid: u32) -> Result<Focus, String>;
    /// Whether selection reading can work at all on this machine.
    fn available(&self) -> Result<(), String>;
}

pub struct Atspi;

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
    let python = crate::tools::which("python3").ok_or("python3 is not installed")?;
    let mut cmd = Command::new(python);
    cmd.args([
        "-I",
        "-c",
        SCRIPT,
        &pid.to_string(),
        &MAX_SELECTION.to_string(),
    ]);
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
}

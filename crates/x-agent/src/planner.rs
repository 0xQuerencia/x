//! Models that turn speech the rules do not understand into registry actions: a local
//! Ollama model, or Gemini. A reply is only ever data: every step must deserialize into
//! [`Action`] and pass validation, or the plan is refused. Nothing a model says runs
//! as a command, and the key for Gemini is never put on a command line or in a log.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::service::Planner;
use crate::tools::{self, Reply};

const OLLAMA_URL: &str = "http://127.0.0.1:11434/api/generate";
const GEMINI_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
pub const GEMINI_DEFAULT: &str = "gemini-2.5-flash";

pub(crate) fn prompt(utterance: &str, smart: bool) -> String {
    let apps: Vec<&str> = tools::APPS.iter().map(|(name, _)| *name).collect();
    let dictation = if smart {
        "If the speech is plain dictation (something to be typed as written, not an instruction \
to the computer), reply {\"dictate\":true}. When unsure, reply {\"dictate\":true}.\n"
    } else {
        ""
    };
    format!(
        "You turn one spoken desktop request into steps. Reply with JSON only: \
{{\"steps\":[...]}}.\n\
Allowed steps:\n\
{{\"kind\":\"app\",\"target\":<one of {apps:?}>}} (opens it)\n\
{{\"kind\":\"focus\",\"target\":<one of {apps:?}>}} (switches to an already open one)\n\
{{\"kind\":\"workspace\",\"number\":1-10}}\n\
{{\"kind\":\"url\",\"target\":\"https://...\"}} (public https only)\n\
{{\"kind\":\"search\",\"target\":\"google|youtube|github|wikipedia|...\",\"query\":\"...\"}}\n\
{{\"kind\":\"type\",\"text\":\"plain text, one line\"}} (types into the window an app step just opened)\n\
{{\"kind\":\"opencode\"}} (opens the OpenCode coding agent interactively in a new terminal and temporary folder; use it \
when they just want an OpenCode session)\n\
{{\"kind\":\"session\",\"goal\":\"...\"}} (a headless coding-agent run in a fresh folder, only when they name a \
concrete task to do; it must be the only step)\n\
{{\"kind\":\"computer\",\"app\":<one of {apps:?}>,\"goal\":\"...\"}} (operates an app that is already open or opens it: reads its \
screen, clicks, types, navigates and checks the result; use it for anything that needs reading or clicking inside an app, \
like choosing a profile, finding something on a page or filling a search box; it must be the only step)\n\
Examples:\n\
Request open a terminal and type hello gives {{\"steps\":[{{\"kind\":\"app\",\"target\":\"terminal\"}},{{\"kind\":\"type\",\"text\":\"hello\"}}]}}\n\
Request look up rust lifetimes on youtube gives {{\"steps\":[{{\"kind\":\"search\",\"target\":\"youtube\",\"query\":\"rust lifetimes\"}}]}}\n\
Request go back to the browser gives {{\"steps\":[{{\"kind\":\"focus\",\"target\":\"firefox\"}}]}}\n\
Every step has a kind of exactly app, focus, workspace, url, search, type, opencode, session or computer.\n\
{dictation}At most {max} steps. You cannot run commands, press Enter, delete, install, send or buy \
anything. If the request needs anything else, reply {{\"steps\":[]}}.\n\
Request: {utterance:?}",
        max = tools::MAX_STEPS
    )
}

/// The JSON object inside a model reply, which may carry stray text around it.
pub(crate) fn extract_object(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (end > start).then(|| &reply[start..=end])
}

pub fn reply_from_text(text: &str, smart: bool) -> Result<Reply, String> {
    let object = extract_object(text).ok_or("The model did not answer with JSON")?;
    match tools::parse_reply(object)? {
        Reply::Dictation if !smart => Err("The model returned dictation for a command".into()),
        reply => Ok(reply),
    }
}

/// Runs curl with its whole configuration on stdin, so nothing secret reaches argv.
fn curl(config: &str) -> Result<serde_json::Value, String> {
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", "45", "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl is needed to reach the model: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "The model did not answer ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("bad model reply: {e}"))
}

/// curl config syntax: a quoted string with `\` and `"` escaped.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// POSTs `body` as JSON. `auth` is one header line, passed to curl on stdin like everything else.
fn post_json(
    url: &str,
    auth: Option<&str>,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut config = format!("url = {}\n", quoted(url));
    if let Some(header) = auth {
        config += &format!("header = {}\n", quoted(header));
    }
    config += &format!(
        "header = \"content-type: application/json\"\ndata = {}\n",
        quoted(&body.to_string())
    );
    curl(&config)
}

pub struct Ollama {
    pub model: String,
}

impl Planner for Ollama {
    fn name(&self) -> String {
        self.model.clone()
    }

    fn is_local(&self) -> bool {
        true
    }

    fn complete(&self, prompt: &str) -> Result<String, String> {
        let body = serde_json::json!({
            "model": self.model, "prompt": prompt,
            "stream": false, "format": "json", "options": { "temperature": 0 },
        });
        let reply = post_json(OLLAMA_URL, None, &body)
            .map_err(|e| format!("{e}. Is `ollama serve` running?"))?;
        if let Some(error) = reply.get("error").and_then(|e| e.as_str()) {
            return Err(error.to_string());
        }
        Ok(reply
            .get("response")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string())
    }
}

/// Gemini over its public API. Only the sentence you spoke and the step list above are
/// sent, never screens or files. The key comes from `GEMINI_API_KEY` or a private file.
pub struct Gemini {
    pub model: String,
    pub key_file: Option<PathBuf>,
}

impl Gemini {
    fn key(&self) -> Result<String, String> {
        let from_env = std::env::var("GEMINI_API_KEY").ok();
        let from_file = self
            .key_file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok());
        from_env
            .or(from_file)
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty() && !key.contains(['\n', '"']))
            .ok_or_else(|| {
                "No Gemini key: set GEMINI_API_KEY or put it in the key file".to_string()
            })
    }
}

impl Planner for Gemini {
    fn name(&self) -> String {
        format!("Gemini {}", self.model)
    }

    fn is_local(&self) -> bool {
        false
    }

    fn complete(&self, prompt: &str) -> Result<String, String> {
        let body = serde_json::json!({
            "contents": [{ "parts": [{ "text": prompt }] }],
            "generationConfig": { "temperature": 0, "responseMimeType": "application/json" },
        });
        let url = format!("{GEMINI_URL}/{}:generateContent", self.model);
        let reply = post_json(
            &url,
            Some(&format!("x-goog-api-key: {}", self.key()?)),
            &body,
        )?;
        if let Some(error) = reply.pointer("/error/message").and_then(|m| m.as_str()) {
            return Err(format!("Gemini: {error}"));
        }
        let answer = reply
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        Ok(answer.to_string())
    }
}

/// Any OpenAI-compatible `/chat/completions` endpoint. The base URL must be https (or
/// loopback); the key comes from a private file and goes to curl on stdin only.
pub struct OpenAi {
    pub model: String,
    pub base_file: PathBuf,
    pub key_file: PathBuf,
}

fn secret(path: &PathBuf) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_string())
        .ok()
        .filter(|text| !text.is_empty() && !text.contains(['\n', '"', '\\']))
        .ok_or_else(|| format!("Missing or invalid {}", path.display()))
}

/// True only for a plain-http base whose host is exactly loopback (no userinfo, no look-alike).
fn is_loopback(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once(':').map_or(authority, |(h, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            authority
        }
    });
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

fn checked_base(base: &str) -> Result<&str, String> {
    let base = base.trim_end_matches('/');
    if base.starts_with("https://") || is_loopback(base) {
        Ok(base)
    } else {
        Err("The endpoint must use https".into())
    }
}

impl Planner for OpenAi {
    fn name(&self) -> String {
        self.model.clone()
    }

    fn is_local(&self) -> bool {
        secret(&self.base_file).is_ok_and(|b| is_loopback(&b))
    }

    fn complete(&self, prompt: &str) -> Result<String, String> {
        let base = secret(&self.base_file)?;
        let url = format!("{}/chat/completions", checked_base(&base)?);
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": prompt }],
            "temperature": 0,
            "response_format": { "type": "json_object" },
        });
        let auth = format!("Authorization: Bearer {}", secret(&self.key_file)?);
        let reply = post_json(&url, Some(&auth), &body)?;
        if let Some(error) = reply.pointer("/error/message").and_then(|m| m.as_str()) {
            return Err(format!("{}: {error}", self.model));
        }
        let answer = reply
            .pointer("/choices/0/message/content")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        Ok(answer.to_string())
    }
}

#[cfg(test)]
mod tests;

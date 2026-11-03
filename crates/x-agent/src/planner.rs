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

fn prompt(utterance: &str, smart: bool) -> String {
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
{{\"kind\":\"session\",\"goal\":\"...\"}} (a temporary coding-agent session in a fresh folder for \
coding, writing or research work; it must be the only step)\n\
Examples:\n\
Request open a terminal and type hello gives {{\"steps\":[{{\"kind\":\"app\",\"target\":\"terminal\"}},{{\"kind\":\"type\",\"text\":\"hello\"}}]}}\n\
Request look up rust lifetimes on youtube gives {{\"steps\":[{{\"kind\":\"search\",\"target\":\"youtube\",\"query\":\"rust lifetimes\"}}]}}\n\
Request go back to the browser gives {{\"steps\":[{{\"kind\":\"focus\",\"target\":\"firefox\"}}]}}\n\
Every step has a kind of exactly app, focus, workspace, url, search, type or session.\n\
{dictation}At most {max} steps. You cannot run commands, press Enter, delete, install, send or buy \
anything. If the request needs anything else, reply {{\"steps\":[]}}.\n\
Request: {utterance:?}",
        max = tools::MAX_STEPS
    )
}

/// The JSON object inside a model reply, which may carry stray text around it.
fn extract_object(reply: &str) -> Option<&str> {
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

pub struct Ollama {
    pub model: String,
}

impl Planner for Ollama {
    fn name(&self) -> String {
        self.model.clone()
    }

    fn plan(&self, text: &str, smart: bool) -> Result<Reply, String> {
        let body = serde_json::json!({
            "model": self.model, "prompt": prompt(text, smart),
            "stream": false, "format": "json", "options": { "temperature": 0 },
        })
        .to_string();
        let config = format!("url = {}\ndata = {}\n", quoted(OLLAMA_URL), quoted(&body));
        let reply = curl(&config).map_err(|e| format!("{e}. Is `ollama serve` running?"))?;
        if let Some(error) = reply.get("error").and_then(|e| e.as_str()) {
            return Err(error.to_string());
        }
        reply_from_text(
            reply.get("response").and_then(|r| r.as_str()).unwrap_or(""),
            smart,
        )
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

    fn plan(&self, text: &str, smart: bool) -> Result<Reply, String> {
        let body = serde_json::json!({
            "contents": [{ "parts": [{ "text": prompt(text, smart) }] }],
            "generationConfig": { "temperature": 0, "responseMimeType": "application/json" },
        })
        .to_string();
        let url = format!("{GEMINI_URL}/{}:generateContent", self.model);
        let config = format!(
            "url = {}\nheader = {}\nheader = \"content-type: application/json\"\ndata = {}\n",
            quoted(&url),
            quoted(&format!("x-goog-api-key: {}", self.key()?)),
            quoted(&body)
        );
        let reply = curl(&config)?;
        if let Some(error) = reply.pointer("/error/message").and_then(|m| m.as_str()) {
            return Err(format!("Gemini: {error}"));
        }
        let answer = reply
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        reply_from_text(answer, smart)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Action;

    #[test]
    fn only_registry_steps_survive() {
        let ok = reply_from_text(
            "sure {\"steps\":[{\"kind\":\"app\",\"target\":\"terminal\"},{\"kind\":\"type\",\"text\":\"hi\"}]} done",
            false,
        )
        .unwrap();
        assert!(matches!(ok, Reply::Steps(steps) if steps.len() == 2));
        for bad in [
            "{\"steps\":[{\"kind\":\"shell\",\"command\":\"rm -rf ~\"}]}",
            "{\"steps\":[{\"kind\":\"app\",\"target\":\"bash\"}]}",
            "{\"steps\":[{\"kind\":\"type\",\"text\":\"a\\nrm -rf ~\\n\"}]}",
            "{\"steps\":[{\"kind\":\"url\",\"target\":\"http://127.0.0.1/admin\"}]}",
            "{\"steps\":[{\"kind\":\"workspace\",\"number\":99}]}",
            "{\"steps\":[{\"kind\":\"session\",\"goal\":\"x\"},{\"kind\":\"app\",\"target\":\"terminal\"}]}",
            "no json here",
        ] {
            assert!(reply_from_text(bad, false).is_err(), "{bad}");
        }
    }

    #[test]
    fn dictation_is_only_accepted_when_asked_for() {
        assert_eq!(
            reply_from_text("{\"dictate\":true}", true),
            Ok(Reply::Dictation)
        );
        assert!(reply_from_text("{\"dictate\":true}", false).is_err());
        assert!(
            prompt("hello", true).contains("dictate")
                && !prompt("hello", false).contains("dictate")
        );
    }

    #[test]
    fn new_steps_parse() {
        let steps = match reply_from_text(
            "{\"steps\":[{\"kind\":\"session\",\"goal\":\"write a poem\"}]}",
            false,
        ) {
            Ok(Reply::Steps(steps)) => steps,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            steps,
            [Action::Session {
                goal: "write a poem".into()
            }]
        );
    }

    #[test]
    fn the_key_never_reaches_argv_and_quotes_are_escaped() {
        assert_eq!(quoted("a\"b\\c"), "\"a\\\"b\\\\c\"");
        let planner = Gemini {
            model: "m".into(),
            key_file: None,
        };
        let _ = planner.name();
    }
}

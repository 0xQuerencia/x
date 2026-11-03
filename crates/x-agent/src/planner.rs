//! A local model (Ollama) that turns speech the rules do not understand into registry
//! actions. It is reached on loopback only, and its answer is parsed as data: every step
//! must deserialize into [`Action`] and pass validation, or the plan is refused.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::service::Planner;
use crate::tools::{self, Action};

const URL: &str = "http://127.0.0.1:11434/api/generate";
const TIMEOUT_SECS: &str = "45";

pub struct Ollama {
    pub model: String,
}

fn prompt(utterance: &str) -> String {
    let apps: Vec<&str> = tools::APPS.iter().map(|(name, _)| *name).collect();
    format!(
        "You turn one spoken desktop request into steps. Reply with JSON only: \
    {{\"steps\":[...]}}.\n\
    Allowed steps:\n\
    {{\"kind\":\"app\",\"target\":<one of {apps:?}>}}\n\
    {{\"kind\":\"url\",\"target\":\"https://...\"}} (public https only)\n\
    {{\"kind\":\"search\",\"target\":\"google|youtube|github|wikipedia|...\",\"query\":\"...\"}}\n\
    {{\"kind\":\"type\",\"text\":\"plain text, one line\"}} (types into the focused window, directly after an app step)\n\
    Examples:\n\
    Request open a terminal and type hello gives {{\"steps\":[{{\"kind\":\"app\",\"target\":\"terminal\"}},{{\"kind\":\"type\",\"text\":\"hello\"}}]}}\n\
    Request look up rust lifetimes on youtube gives {{\"steps\":[{{\"kind\":\"search\",\"target\":\"youtube\",\"query\":\"rust lifetimes\"}}]}}\n\
    Every step has a kind of exactly app, url, search or type.\n\
    At most {max} steps. You cannot run commands, press Enter, delete, install, send or buy anything. \
    If the request needs anything else, reply {{\"steps\":[]}}.\n\
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

pub fn actions_from_reply(reply: &str) -> Result<Vec<Action>, String> {
    let object = extract_object(reply).ok_or("The model did not answer with JSON")?;
    tools::parse_plan_json(object)
}

impl Planner for Ollama {
    fn name(&self) -> String {
        self.model.clone()
    }

    fn plan(&self, text: &str) -> Result<Vec<Action>, String> {
        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt(text),
            "stream": false,
            "format": "json",
            "options": { "temperature": 0 },
        })
        .to_string();
        let mut child = Command::new("curl")
            .args(["-sS", "--max-time", TIMEOUT_SECS, "-d", "@-", URL])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("curl is needed to reach Ollama: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(body.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "Ollama did not answer ({}). Is `ollama serve` running?",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let reply: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("bad Ollama reply: {e}"))?;
        if let Some(error) = reply.get("error").and_then(|e| e.as_str()) {
            return Err(error.to_string());
        }
        let text = reply.get("response").and_then(|r| r.as_str()).unwrap_or("");
        actions_from_reply(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_registry_steps_survive() {
        let ok = actions_from_reply(
            "sure {\"steps\":[{\"kind\":\"app\",\"target\":\"terminal\"},{\"kind\":\"type\",\"text\":\"hi\"}]} done",
        )
        .unwrap();
        assert_eq!(ok.len(), 2);
        for bad in [
            "{\"steps\":[{\"kind\":\"shell\",\"command\":\"rm -rf ~\"}]}",
            "{\"steps\":[{\"kind\":\"app\",\"target\":\"bash\"}]}",
            "{\"steps\":[{\"kind\":\"type\",\"text\":\"a\\nrm -rf ~\\n\"}]}",
            "{\"steps\":[{\"kind\":\"url\",\"target\":\"http://127.0.0.1/admin\"}]}",
            "no json here",
        ] {
            assert!(actions_from_reply(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_prompt_lists_the_closed_tool_set() {
        let p = prompt("make me coffee");
        assert!(
            p.contains("terminal") && p.contains("\"steps\":[]") && p.contains("make me coffee")
        );
    }
}

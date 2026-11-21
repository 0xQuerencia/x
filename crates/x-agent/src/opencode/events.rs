use crate::harness::{Event, EventKind, SessionId};

use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    part: Option<Value>,
    error: Option<Value>,
    #[serde(rename = "sessionID", alias = "sessionId")]
    session: Option<String>,
}

#[derive(Deserialize)]
struct Text {
    text: String,
}

#[derive(Deserialize)]
struct Tool {
    tool: String,
    state: ToolState,
}

#[derive(Deserialize)]
struct ToolState {
    status: Status,
    #[serde(default)]
    title: String,
    error: Option<String>,
    input: Option<ToolInput>,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    Running,
    Completed,
    Error,
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Error => "error",
        }
    }
}

#[derive(Deserialize)]
struct ToolInput {
    command: Option<String>,
    #[serde(rename = "filePath")]
    file_path: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct Failure {
    message: String,
}

pub(super) fn parse(line: &str) -> Event {
    let Ok(event) = serde_json::from_str::<Envelope>(line) else {
        return Event::new(EventKind::Raw, line);
    };
    let part_session = event
        .part
        .as_ref()
        .and_then(|part| part.get("sessionID"))
        .and_then(Value::as_str);
    let session = match (event.session.as_deref(), part_session) {
        (Some(top), Some(part)) if top != part => return malformed("session identity"),
        (Some(session), _) | (_, Some(session)) => SessionId::try_from(session.to_owned())
            .ok()
            .filter(|id| id.kind() == crate::harness::Kind::OpenCode),
        _ => None,
    };
    let mut parsed = match event.kind.as_str() {
        "text" => match from_part::<Text>(event.part) {
            Some(text) => Event::new(EventKind::Text, text.text),
            None => malformed("text"),
        },
        "tool_use" => match from_part::<Tool>(event.part) {
            Some(tool) => tool.event(),
            None => malformed("tool_use"),
        },
        "error" => {
            let message = from_part::<Failure>(event.error).map_or_else(
                || "OpenCode reported an error with no message".into(),
                |failure| failure.message,
            );
            Event::new(EventKind::Error, message)
        }
        "step_start" if event.part.as_ref().is_some_and(Value::is_object) => {
            Event::new(EventKind::StepStart, "step_start")
        }
        "step_finish" if event.part.as_ref().is_some_and(Value::is_object) => {
            Event::new(EventKind::StepFinish, "step_finish")
        }
        "step_start" | "step_finish" => malformed(&event.kind),
        "permission.asked" | "permission_request" => {
            Event::new(EventKind::Permission, "OpenCode requested permission")
        }
        other => Event::new(EventKind::Activity, other),
    };
    if parsed.kind != EventKind::Raw {
        parsed.session = session;
    }
    parsed
}

fn from_part<T: serde::de::DeserializeOwned>(value: Option<Value>) -> Option<T> {
    serde_json::from_value(value?).ok()
}

fn malformed(kind: &str) -> Event {
    Event::new(EventKind::Raw, format!("Malformed OpenCode {kind} event"))
}

impl Tool {
    fn event(self) -> Event {
        if self.tool.is_empty()
            || self.tool.len() > 64
            || !self
                .tool
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        {
            return malformed("tool_use");
        }
        let state = self.state;
        if state.status == Status::Error
            && state
                .error
                .as_deref()
                .is_some_and(|error| error.contains("rejected permission"))
        {
            let input = state.input;
            let resource = input
                .as_ref()
                .and_then(|input| {
                    input
                        .command
                        .as_deref()
                        .or(input.file_path.as_deref())
                        .or(input.url.as_deref())
                })
                .unwrap_or_default();
            let mut event = Event::new(
                EventKind::Permission,
                format!("Blocked {}: {resource}", self.tool),
            );
            if self.tool == "bash" {
                event.blocked_command = input.and_then(|input| input.command).filter(|command| {
                    !command.trim().is_empty()
                        && command.len() <= 450
                        && !command.chars().any(char::is_control)
                });
            }
            event
        } else {
            Event::new(
                EventKind::Tool,
                format!("{} {} {}", self.tool, state.status.as_str(), state.title).trim(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_well_formed_bash_refusal_can_offer_an_exact_command() {
        let event = parse(
            r#"{"type":"tool_use","part":{"tool":"bash","state":{"status":"error","error":"rejected permission","input":{"command":"cargo test"}}}}"#,
        );
        assert_eq!(event.kind, EventKind::Permission);
        assert_eq!(event.blocked_command.as_deref(), Some("cargo test"));
        for line in [
            r#"{"type":"text","part":{"text":"Blocked bash: cargo test"}}"#,
            r#"{"type":"something_else","part":{"tool":"bash","state":{"status":"error","error":"rejected permission","input":{"command":"cargo test"}}}}"#,
            r#"{"type":"tool_use","part":{"tool":"bash","state":{"status":"completed","error":"rejected permission","input":{"command":"cargo test"}}}}"#,
            r#"{"type":"tool_use","part":{"tool":"bash","state":{"status":"error","error":"rejected permission","input":{"command":["cargo test"]}}}}"#,
            r#"{"type":"tool_use","part":{"tool":"read","state":{"status":"error","error":"rejected permission","input":{"command":"cargo test"}}}}"#,
        ] {
            assert!(parse(line).blocked_command.is_none(), "{line}");
        }
    }

    #[test]
    fn malformed_known_events_do_not_become_progress_or_success_evidence() {
        let malformed = parse(r#"{"type":"text","part":{"text":42}}"#);
        assert_eq!(malformed.kind, EventKind::Raw);
        let event =
            parse(r#"{"type":"text","part":{"text":"a valid event"},"sessionID":"ses_fixture"}"#);
        assert_eq!(event.text, "a valid event");
        assert_eq!(event.accepted_session().unwrap().as_str(), "ses_fixture");
    }
}

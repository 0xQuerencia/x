use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use super::{
    Adapter as HarnessAdapter, Capability, Descriptor, Event, EventKind, EventSink, Kind, OwnedRun,
    Request, SessionId, probe, process,
};
use crate::cancellation::Cancellation;

pub const CAPABILITIES: &[Capability] = &[
    Capability::OwnedRun,
    Capability::StreamEvents,
    Capability::StopOwnedRun,
    Capability::ResumeOwnedSession,
    Capability::ToolFreeAnalysis,
];

pub struct Adapter {
    exe: PathBuf,
}

impl Adapter {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe }
    }
}

impl HarnessAdapter for Adapter {
    fn descriptor(&self) -> Descriptor {
        Descriptor {
            kind: Kind::ClaudeCode,
            event_schema: 2,
            version: None,
            capabilities: CAPABILITIES,
        }
    }

    fn start(
        &self,
        request: Request,
        events: EventSink,
        stop: &Cancellation,
    ) -> Result<Box<dyn OwnedRun>, String> {
        stop.check()?;
        if !request.allow_commands.is_empty() {
            return Err("Claude Code Work in X does not accept command grants".into());
        }
        if request
            .session
            .as_ref()
            .is_some_and(|id| id.kind() != Kind::ClaudeCode)
        {
            return Err("The selected session does not belong to Claude Code".into());
        }
        let Some((true, help)) = probe::query(&self.exe, &["--help"]) else {
            return Err("Could not probe the Claude Code CLI".into());
        };
        if !supports_tool_free_output(&help) {
            return Err(
                "The installed Claude Code does not expose X's structured, tool-free mode".into(),
            );
        }
        let version = probe::query(&self.exe, &["--version"])
            .filter(|(success, _)| *success)
            .and_then(|(_, output)| probe::parse_version(Kind::ClaudeCode, &output));
        if !version.is_some_and(|version| version.starts_with("2.")) {
            return Err(
                "X needs Claude Code 2 with structured output and tool restrictions".into(),
            );
        }
        if let Some((success, output)) = probe::query(&self.exe, &["auth", "status", "--json"])
            && probe::auth(Kind::ClaudeCode, success, &output) == probe::Auth::NeedsLogin
        {
            return Err("Claude Code reports that login is required".into());
        }
        let mut cmd = Command::new(&self.exe);
        cmd.args([
            "--print",
            "--verbose",
            "--output-format",
            "stream-json",
            "--permission-mode",
            "default",
            "--tools",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            r#"{"mcpServers":{}}"#,
            "--setting-sources",
            "",
            "--disable-slash-commands",
        ]);
        if let Some(session) = &request.session {
            cmd.args(["--resume", session.as_str()]);
        }
        cmd.args(["--", &request.prompt]);
        let spec = process::Spec {
            name: "Claude Code",
            workspace: request.workspace,
            stall_after: Duration::from_secs(120),
            session: request.session,
            require_terminal: true,
            parse,
        };
        Ok(Box::new(process::Run::start(spec, cmd, events, stop)?))
    }
}

pub(crate) fn supports_tool_free_output(help: &str) -> bool {
    let required = [
        "--print",
        "--output-format",
        "--resume",
        "--permission-mode",
        "--tools",
        "--strict-mcp-config",
        "--mcp-config",
        "--setting-sources",
        "--disable-slash-commands",
    ];
    help.contains("stream-json") && required.iter().all(|name| probe::has_flag(help, name))
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    session_id: Option<String>,
    subtype: Option<String>,
    is_error: Option<bool>,
    result: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    content: Vec<Content>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

fn parse(line: &str) -> Event {
    let Ok(envelope) = serde_json::from_str::<Envelope>(line) else {
        return Event::new(EventKind::Raw, "Non-structured Claude Code output");
    };
    let session = envelope
        .session_id
        .and_then(|id| SessionId::try_from(id).ok())
        .filter(|id| id.kind() == Kind::ClaudeCode);
    let mut event = match envelope.kind.as_str() {
        "assistant" => {
            let text = envelope
                .message
                .map(|message| {
                    message
                        .content
                        .into_iter()
                        .filter_map(|part| if part.kind == "text" { part.text } else { None })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if text.is_empty() {
                Event::new(EventKind::Activity, "Claude Code assistant event")
            } else {
                Event::new(EventKind::Text, text)
            }
        }
        "result"
            if envelope.is_error == Some(false)
                && envelope.subtype.as_deref() == Some("success")
                && session.is_some() =>
        {
            Event::new(
                EventKind::StepFinish,
                envelope
                    .result
                    .unwrap_or_else(|| "Claude Code completed".into()),
            )
        }
        "result" => Event::new(
            EventKind::Error,
            "Claude Code did not return a successful result",
        ),
        "error" => Event::new(EventKind::Error, "Claude Code reported an error"),
        "system" => Event::new(EventKind::Activity, "Claude Code system event"),
        _ => Event::new(EventKind::Activity, "Claude Code session event"),
    };
    event.session = session;
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_free_claude_does_not_advertise_workspace_writes() {
        let adapter = Adapter::new(PathBuf::from("unused"));
        let descriptor = adapter.descriptor();
        assert!(
            descriptor
                .capabilities
                .contains(&Capability::ToolFreeAnalysis)
        );
        assert!(!descriptor.supports_workspace_write());
    }

    #[test]
    fn terminal_result_and_identity_are_required_for_success_evidence() {
        let id = "5e3b19e7-c7ce-42f0-919d-15a735a1ed16";
        let complete = parse(&format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"{id}"}}"#
        ));
        assert_eq!(complete.kind, EventKind::StepFinish);
        assert_eq!(complete.session.unwrap().as_str(), id);
        for malformed in [
            r#"{"type":"result","subtype":"success","is_error":false}"#,
            r#"{"type":"result","subtype":"error","is_error":true}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"ses_an_opencode_id"}"#,
        ] {
            assert_eq!(parse(malformed).kind, EventKind::Error);
        }
    }
}

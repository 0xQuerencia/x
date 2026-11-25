use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::harness::{Kind, Session, SessionId};

const LIST_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub(super) struct SessionList {
    workspace: PathBuf,
    listed: Instant,
    sessions: Vec<Session>,
}

pub(super) fn spoken_target(text: &str) -> Option<String> {
    let text = crate::tools::strip_wake_word(text);
    let text = text.trim_end_matches(['.', '!', '?']).trim();
    let lower = text.to_ascii_lowercase();
    for prefix in ["give this to session", "send this to session"] {
        if let Some(tail) = lower.strip_prefix(prefix)
            && (tail.is_empty() || tail.starts_with(' '))
        {
            return Some(text[prefix.len()..].trim().to_string());
        }
    }
    None
}

pub(super) fn spoken_work(text: &str) -> Option<(Kind, String)> {
    let text = crate::tools::strip_wake_word(text);
    let lower = text.to_ascii_lowercase();
    for (prefix, kind) in [
        ("ask opencode to", Kind::OpenCode),
        ("ask open code to", Kind::OpenCode),
        ("ask claude code to", Kind::ClaudeCode),
    ] {
        if let Some(tail) = lower.strip_prefix(prefix)
            && (tail.is_empty() || tail.starts_with(' '))
        {
            return Some((kind, text[prefix.len()..].trim().to_string()));
        }
    }
    None
}

fn ordinal(selector: &str) -> Option<usize> {
    if let Ok(number) = selector.parse() {
        return Some(number);
    }
    [
        ["one", "first"],
        ["two", "second"],
        ["three", "third"],
        ["four", "fourth"],
        ["five", "fifth"],
        ["six", "sixth"],
        ["seven", "seventh"],
        ["eight", "eighth"],
        ["nine", "ninth"],
        ["ten", "tenth"],
    ]
    .iter()
    .position(|names| names.contains(&selector))
    .map(|index| index + 1)
}

fn normalize(title: &str) -> String {
    title
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

impl SessionList {
    pub(super) fn new(workspace: PathBuf, sessions: Vec<Session>, listed: Instant) -> Self {
        Self {
            workspace,
            listed,
            sessions,
        }
    }

    pub(super) fn resolve(
        &self,
        workspace: &Path,
        selector: &str,
        now: Instant,
    ) -> Result<Session, String> {
        if self.workspace != workspace {
            return Err("List this project's sessions again before using a number or title".into());
        }
        if selector.starts_with("ses_") {
            let id = SessionId::try_from(selector.to_string())?;
            return self
                .sessions
                .iter()
                .find(|session| session.id == id)
                .cloned()
                .ok_or_else(|| "That exact session is absent from the displayed list".into());
        }
        if now.saturating_duration_since(self.listed) > LIST_TTL {
            return Err("List this project's sessions again before using a number or title".into());
        }
        let selector = normalize(selector);
        if let Some(number) = ordinal(&selector) {
            return number
                .checked_sub(1)
                .and_then(|index| self.sessions.get(index))
                .cloned()
                .ok_or_else(|| "No session has that number in the displayed list".into());
        }
        if selector.is_empty() {
            return Err("Give a session number, displayed title or exact ID".into());
        }
        let mut matches = self
            .sessions
            .iter()
            .filter(|session| normalize(&session.title) == selector);
        let target = matches
            .next()
            .ok_or("No displayed session matches that title")?;
        if matches.next().is_some() {
            return Err(
                "Several displayed sessions have that title. Give an exact ID or number.".into(),
            );
        }
        Ok(target.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, title: &str, workspace: &Path) -> Session {
        Session {
            id: SessionId::try_from(id.to_string()).unwrap(),
            title: title.into(),
            workspace: workspace.to_owned(),
            updated_at: 0,
        }
    }

    #[test]
    fn aliases_and_numbers_use_the_same_visible_snapshot_until_expiry() {
        let workspace = PathBuf::from("/fixture/project");
        let listed = Instant::now();
        let first = session("ses_first", "Review tests", &workspace);
        let second = session("ses_second", "review  tests", &workspace);
        let catalog = SessionList::new(
            workspace.clone(),
            vec![first.clone(), second.clone()],
            listed,
        );

        assert_eq!(catalog.resolve(&workspace, "one", listed).unwrap(), first);
        assert_eq!(catalog.resolve(&workspace, "2", listed).unwrap(), second);
        assert_eq!(
            catalog.resolve(&workspace, "ses_second", listed).unwrap(),
            second
        );
        assert!(
            catalog
                .resolve(&workspace, "Review tests", listed)
                .unwrap_err()
                .contains("Several")
        );
        assert!(catalog.resolve(&workspace, "0", listed).is_err());
        assert!(
            catalog
                .resolve(
                    &workspace,
                    "one",
                    listed + LIST_TTL + Duration::from_secs(1)
                )
                .is_err()
        );
        assert!(
            catalog
                .resolve(Path::new("/fixture/other"), "one", listed)
                .is_err()
        );
    }

    #[test]
    fn direct_harness_speech_preserves_the_goal() {
        assert_eq!(
            spoken_work("Friday, ask OpenCode to Review the API tests"),
            Some((Kind::OpenCode, "Review the API tests".into())),
        );
        assert_eq!(
            spoken_work("ask Claude Code to explain the error"),
            Some((Kind::ClaudeCode, "explain the error".into())),
        );
        assert!(spoken_work("we could ask opencode to check this").is_none());
        assert!(spoken_work("ask opencode tomorrow").is_none());
    }

    #[test]
    fn recognizes_the_explicit_spoken_handoff_only() {
        assert_eq!(
            spoken_target("Friday, give this to session one."),
            Some("one".into())
        );
        assert_eq!(
            spoken_target("send this to session ses_A"),
            Some("ses_A".into())
        );
        assert!(spoken_target("I asked you to give this to session one").is_none());
    }
}

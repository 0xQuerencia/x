use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionId(String);

impl SessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SessionId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = value.strip_prefix("ses_").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.len() <= 120
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
        if valid {
            Ok(Self(value))
        } else {
            Err("Enter an exact OpenCode session ID beginning with ses_")
        }
    }
}

impl From<SessionId> for String {
    fn from(id: SessionId) -> Self {
        id.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub title: String,
    pub workspace: PathBuf,
    pub updated_at: u64,
}

impl Session {
    pub fn from_open_code(
        id: String,
        title: &str,
        reported_directory: &Path,
        workspace: &Path,
        updated_at: u64,
    ) -> Option<Self> {
        if !reported_directory.is_absolute()
            || reported_directory.canonicalize().ok()?.as_path() != workspace
        {
            return None;
        }
        Some(Self {
            id: SessionId::try_from(id).ok()?,
            title: title
                .chars()
                .filter(|c| !c.is_control())
                .take(160)
                .collect(),
            workspace: workspace.to_owned(),
            updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_stale_directory_and_invalid_ids() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        let a = Session::from_open_code("ses_a".into(), "title\u{1b}", &workspace, &workspace, 10)
            .unwrap();
        assert_eq!(a.title, "title");
        assert_eq!(a.id.as_str(), "ses_a");
        for bad in ["ses_", "ses_a/next", "ses_%0d", "window title", "2"] {
            assert!(SessionId::try_from(bad.to_string()).is_err());
        }
        assert!(
            Session::from_open_code(
                "ses_a".into(),
                "title",
                &workspace.join("gone"),
                &workspace,
                0,
            )
            .is_none()
        );
    }
}

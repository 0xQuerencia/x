//! Deterministic, local search over X's already supported desktop actions and workflows.
//! This is a discovery layer, not a second executor or an LLM-powered planner.

use crate::tools;

const MAX_RESULTS: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    OpenApp(String),
    FocusApp(String),
    Workspace(u8),
    Workflow(String),
}

impl Target {
    /// Built-in actions use the existing typed rule planner and approval path.
    /// Workflows use the workflow engine instead.
    pub fn phrase(&self) -> Option<String> {
        match self {
            Self::OpenApp(app) => Some(format!("open {app}")),
            Self::FocusApp(app) => Some(format!("focus {app}")),
            Self::Workspace(n) => Some(format!("switch to workspace {n}")),
            Self::Workflow(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hit {
    pub title: String,
    pub detail: String,
    pub target: Target,
}

fn display_app(app: &str) -> &str {
    match app {
        "firefox" => "Firefox",
        "chrome" => "Google Chrome",
        "chromium" => "Chromium",
        "brave" => "Brave",
        "vscode" => "Visual Studio Code",
        "terminal" => "Terminal",
        "files" => "File manager",
        other => other,
    }
}

/// Search never asks a language model to guess which action was meant.
/// Terms must all match a title or description; prefix/substring hits beat
/// subsequence matches. Scoring is deliberately stable and cheap.
fn term_score(needle: &str, haystack: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack == needle {
        return Some(0);
    }
    if haystack.starts_with(needle) {
        return Some(10);
    }
    if let Some(at) = haystack.find(needle) {
        return Some(40 + at);
    }
    let mut last = None;
    let mut gaps = 0;
    let mut chars = haystack.chars().enumerate();
    for ch in needle.chars() {
        let (index, _) = chars.find(|(_, candidate)| *candidate == ch)?;
        if let Some(previous) = last {
            gaps += index - previous - 1;
        }
        last = Some(index);
    }
    Some(200 + gaps)
}

fn score(query: &str, hit: &Hit) -> Option<usize> {
    let title = hit.title.to_lowercase();
    let details = hit.detail.to_lowercase();
    let mut sum = 0usize;
    for term in query.to_lowercase().split_whitespace() {
        let best =
            term_score(term, &title).or_else(|| term_score(term, &details).map(|n| n + 80))?;
        sum += best;
    }
    Some(sum)
}

/// `workflows` contains only runnable, validated workflows. Their identifiers
/// are not treated as free-form commands and are handed to the existing engine.
pub fn search(query: &str, workflows: impl IntoIterator<Item = (String, String)>) -> Vec<Hit> {
    let mut catalog = Vec::new();
    for (id, name) in workflows {
        catalog.push(Hit {
            title: name,
            detail: "Saved workflow".into(),
            target: Target::Workflow(id),
        });
    }
    for (app, _) in tools::APPS {
        let title = display_app(app);
        catalog.push(Hit {
            title: format!("Open {title}"),
            detail: format!("Launch {app}"),
            target: Target::OpenApp((*app).into()),
        });
    }
    for (app, _) in tools::APPS {
        let title = display_app(app);
        catalog.push(Hit {
            title: format!("Focus {title}"),
            detail: format!("Switch to an existing {app} window"),
            target: Target::FocusApp((*app).into()),
        });
    }
    for n in 1..=10 {
        catalog.push(Hit {
            title: format!("Switch to workspace {n}"),
            detail: format!("Workspace {n}"),
            target: Target::Workspace(n),
        });
    }
    if query.trim().is_empty() {
        return catalog.into_iter().take(MAX_RESULTS).collect();
    }
    let mut ranked: Vec<_> = catalog
        .into_iter()
        .filter_map(|hit| score(query, &hit).map(|score| (score, hit)))
        .collect();
    ranked.sort_by(|(a_score, a), (b_score, b)| {
        a_score.cmp(b_score).then_with(|| a.title.cmp(&b.title))
    });
    ranked
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, hit)| hit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_app_actions_are_reused_not_reimplemented() {
        for hit in search("", std::iter::empty()) {
            if let Some(phrase) = hit.target.phrase() {
                assert!(tools::plan_rules(&phrase).is_some(), "{phrase}");
            }
        }
        for hit in search("focus vscode", std::iter::empty()) {
            if let Some(phrase) = hit.target.phrase() {
                assert!(tools::plan_rules(&phrase).is_some(), "{phrase}");
            }
        }
    }

    #[test]
    fn project_workflows_are_discoverable_without_model_calls() {
        let hits = search("coding", [("coding-setup".into(), "Start coding".into())]);
        assert_eq!(hits[0].target, Target::Workflow("coding-setup".into()));
        assert_eq!(hits[0].target.phrase(), None);
    }

    #[test]
    fn prefix_beats_subsequence_and_result_count_is_bounded() {
        let hits = search("open firefox", std::iter::empty());
        assert_eq!(hits[0].target, Target::OpenApp("firefox".into()));
        assert!(hits.len() <= MAX_RESULTS);
        assert!(search("", std::iter::empty()).len() <= MAX_RESULTS);
    }

    #[test]
    fn unmatched_query_does_not_make_up_actions() {
        assert!(search("this command does not exist", std::iter::empty()).is_empty());
    }

    #[test]
    fn workspace_phrases_are_checked_by_existing_parser() {
        let hits = search("workspace 3", std::iter::empty());
        assert!(hits.iter().any(|h| h.target == Target::Workspace(3)));
        for n in 1..=10 {
            assert!(tools::plan_rules(&Target::Workspace(n).phrase().unwrap()).is_some());
        }
    }
}

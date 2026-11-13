//! Deterministic, local search over X's already supported desktop actions and workflows.
//! This is a discovery layer, not a second executor or an LLM-powered planner.

use crate::tools;

pub mod clipboard_history;
mod colors;
pub mod datetime;
pub mod files;
mod math;
pub mod picker;
pub mod processes;
pub mod snippets;

const MAX_RESULTS: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    OpenApp(String),
    FocusApp(String),
    Workspace(u8),
    Workflow(String),
    WebSearch {
        site: String,
        query: String,
    },
    /// A pure local result explicitly copied by the user, never executed as a command.
    CopyResult(String),
    PickColor,
    OpenFile(String),
    EndProcess(processes::ProcessInfo),
}

impl Target {
    /// Built-in actions use the existing typed rule planner and approval path.
    /// Workflows use the workflow engine instead.
    pub fn phrase(&self) -> Option<String> {
        match self {
            Self::OpenApp(app) => Some(format!("open {app}")),
            Self::FocusApp(app) => Some(format!("focus {app}")),
            Self::Workspace(n) => Some(format!("switch to workspace {n}")),
            Self::WebSearch { site, query } => Some(format!("search {site} for {query}")),
            Self::Workflow(_)
            | Self::CopyResult(_)
            | Self::PickColor
            | Self::OpenFile(_)
            | Self::EndProcess(_) => None,
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

/// A web-search suggestion uses the same verified action parser as voice
/// requests. Prefixes are deliberately explicit; ordinary queries search
/// Google. Never invoke an LLM or construct a URL directly from a query.
fn web_search(query: &str) -> Option<Hit> {
    // Preserve trailing whitespace until a provider prefix is parsed:
    // "g " is an incomplete search, not a Google search for the letter g.
    let raw = query.trim_start();
    if raw.trim().is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    // A local utility query must not acquire a tempting accidental web
    // fallback. Clipboard/history/snippet terms may contain private data.
    if [
        "history ",
        "hist ",
        "clipboard ",
        "clip ",
        "snip ",
        "snippet ",
        "file ",
        "f ",
        "process ",
        "ps ",
        "calc ",
        "color ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return None;
    }
    let (site, terms) = [
        ("gh ", "github"),
        ("yt ", "youtube"),
        ("g ", "google"),
        ("w ", "wikipedia"),
        ("r ", "reddit"),
        ("so ", "stackoverflow"),
    ]
    .iter()
    .find_map(|(prefix, site)| {
        lower
            .starts_with(*prefix)
            // then_some() eagerly slices even for a nonmatching prefix.
            // Then use a closure so single-letter/Unicode queries never panic.
            .then(|| (*site, &raw[prefix.len()..]))
    })
    .unwrap_or(("google", raw));
    let terms = terms.trim();
    if terms.is_empty() || terms.chars().count() > 300 || terms.chars().any(char::is_control) {
        return None;
    }
    let target = Target::WebSearch {
        site: site.into(),
        query: terms.into(),
    };
    let phrase = target.phrase()?;
    // Keep search routing inside the existing typed action registry, including
    // its URL encoding and query limits; invalid input gets no suggestion.
    if !matches!(
        tools::plan_rules(&phrase).as_deref(),
        Some([tools::Action::Search { .. }])
    ) {
        return None;
    }
    let title = format!(
        "Search {} for \"{terms}\"",
        match site {
            "github" => "GitHub",
            "youtube" => "YouTube",
            "google" => "Google",
            "wikipedia" => "Wikipedia",
            "reddit" => "Reddit",
            "stackoverflow" => "Stack Overflow",
            _ => return None,
        }
    );
    Some(Hit {
        title,
        detail: "Web search · uses the existing Actions approval".into(),
        target,
    })
}

/// High-signal local utilities run without a model or shell and cannot
/// execute arbitrary code. They only offer a copyable result on demand.
fn local_results(query: &str) -> Vec<Hit> {
    let trimmed = query.trim();
    let mut results = Vec::new();
    for file in files::search(trimmed) {
        results.push(Hit {
            title: file.name,
            detail: format!("Local file · {}", file.path),
            target: Target::OpenFile(file.path),
        });
    }
    if picker::available()
        && (trimmed.eq_ignore_ascii_case("pick color")
            || trimmed.eq_ignore_ascii_case("color picker")
            || trimmed.eq_ignore_ascii_case("pick"))
    {
        results.push(Hit {
            title: "Pick a color from the screen".into(),
            detail: "Requires hyprpicker · click to select one pixel".into(),
            target: Target::PickColor,
        });
    }
    if let Some((label, value)) = datetime::now(trimmed) {
        results.push(Hit {
            title: format!("{label}: {value}"),
            detail: "Calculated locally · no timezone/DST database · copy result".into(),
            target: Target::CopyResult(value),
        });
    }
    let expression = trimmed
        .strip_prefix('=')
        .or_else(|| trimmed.strip_prefix("calc "));
    if let Some(expression) = expression
        && let Some(value) = math::calculate(expression)
    {
        results.push(Hit {
            title: format!("= {value}"),
            detail: "Calculated locally · copy result".into(),
            target: Target::CopyResult(value),
        });
    }
    let process_filter = trimmed
        .strip_prefix("process ")
        .or_else(|| trimmed.strip_prefix("ps "))
        .or_else(|| {
            (trimmed == "processes" || trimmed == "process" || trimmed == "ps").then_some("")
        });
    if let Some(filter) = process_filter {
        for process in processes::list(filter) {
            results.push(Hit {
                title: format!("{} (PID {})", process.name, process.pid),
                detail: format!(
                    "{} MiB resident · end only after confirmation",
                    process.resident_kib / 1024
                ),
                target: Target::EndProcess(process),
            });
        }
    }
    if let Some(formats) = colors::convert(trimmed) {
        for (name, value) in [
            ("HEX", formats.hex),
            ("RGB", formats.rgb),
            ("HSL", formats.hsl),
            ("OKLCH", formats.oklch),
        ] {
            results.push(Hit {
                title: format!("{name}: {value}"),
                detail: "Converted locally · copy result".into(),
                target: Target::CopyResult(value),
            });
        }
    }
    results
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
    let mut results: Vec<_> = ranked
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, hit)| hit)
        .collect();
    if let Some(web) = web_search(query) {
        // Search fallback: the last result is always an explicit search
        // option. Don't hide an app or workflow hit unless the list is full.
        results.truncate(MAX_RESULTS - 1);
        results.push(web);
    }
    let mut local = local_results(query);
    if let Ok(entries) = clipboard_history::matches(query) {
        for entry in entries {
            let preview = entry.text.chars().take(72).collect::<String>();
            local.push(Hit {
                title: format!("Clipboard: {preview}"),
                detail: "Saved explicitly · stored locally · copy on request".into(),
                target: Target::CopyResult(entry.text),
            });
        }
    }
    if let Ok(snippets) = snippets::matches(query) {
        for snippet in snippets {
            local.push(Hit {
                title: format!("Snippet: {}", snippet.trigger),
                detail: "Saved locally · copy text on request".into(),
                target: Target::CopyResult(snippet.text),
            });
        }
    }
    local.extend(results);
    local.truncate(MAX_RESULTS);
    local
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
    fn private_local_queries_do_not_offer_web_search_fallback() {
        for query in [
            "hist private conversation",
            "clip secret",
            "snip signature",
            "file contract",
        ] {
            assert!(web_search(query).is_none(), "{query}");
        }
    }

    #[test]
    fn unmatched_query_offers_only_an_explicit_web_search() {
        let hits = search("this command does not exist", std::iter::empty());
        assert_eq!(hits.len(), 1);
        assert!(matches!(hits[0].target, Target::WebSearch { .. }));
    }

    #[test]
    fn web_search_fallback_reuses_existing_search_action() {
        let hits = search("rust desktop automation", std::iter::empty());
        let last = hits.last().unwrap();
        assert_eq!(
            last.target,
            Target::WebSearch {
                site: "google".into(),
                query: "rust desktop automation".into(),
            }
        );
        assert!(matches!(
            tools::plan_rules(&last.target.phrase().unwrap()).as_deref(),
            Some([tools::Action::Search { .. }])
        ));
    }

    #[test]
    fn web_prefixes_select_the_existing_approved_search_sites() {
        for (text, expected_site) in [
            ("gh rust", "github"),
            ("yt parakeet", "youtube"),
            ("g linux", "google"),
            ("w wayland", "wikipedia"),
            ("r rust", "reddit"),
            ("so rust", "stackoverflow"),
        ] {
            assert!(
                matches!(search(text, std::iter::empty()).last().unwrap().target,
                    Target::WebSearch { ref site, .. } if site == expected_site
                )
            );
        }
    }

    #[test]
    fn short_and_unicode_queries_never_slice_past_input() {
        for query in ["a", "g", "é", "🔎", "q", "x"] {
            let _ = search(query, std::iter::empty());
        }
    }

    #[test]
    fn web_query_is_bounded_and_cannot_inject_control_characters() {
        assert!(web_search("g ").is_none());
        assert!(web_search(&format!("g {}", "a".repeat(301))).is_none());
        assert!(web_search("g hello\\nworld").is_some());
        assert!(web_search("g hello\nworld").is_none());
    }

    #[test]
    fn calculator_is_a_bounded_local_result_not_an_action() {
        let hits = search("=2 * (3 + 4)", std::iter::empty());
        assert_eq!(hits[0].target, Target::CopyResult("14".into()));
        assert_eq!(hits[0].target.phrase(), None);
        assert!(
            !search("=0/0", std::iter::empty())
                .iter()
                .any(|h| matches!(h.target, Target::CopyResult(_)))
        );
    }

    #[test]
    fn color_values_are_available_in_four_formats() {
        let hits = search("color #abc", std::iter::empty());
        assert_eq!(hits[0].target, Target::CopyResult("#AABBCC".into()));
        assert_eq!(
            hits[1].target,
            Target::CopyResult("rgb(170, 187, 204)".into())
        );
        assert!(hits.iter().any(|h| h.title.starts_with("OKLCH:")));
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

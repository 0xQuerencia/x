//! On-device, source-backed help for X Quick.
//! Documentation is embedded at build time and never sent to a web service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Match {
    pub title: String,
    pub source: &'static str,
    pub excerpt: String,
}

const SOURCES: &[(&str, &str)] = &[
    ("X actions", include_str!("../../../../docs/features/x-agent.md")),
    ("CLI", include_str!("../../../../docs/cli.md")),
    ("Vocabulary", include_str!("../../../../docs/features/vocabulary.md")),
    ("History", include_str!("../../../../docs/features/transcript-cli.md")),
];

fn section_excerpt(lines: &[&str]) -> String {
    let mut out = String::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with("<!--") || line.starts_with("    ") {
            continue;
        }
        if !out.is_empty() { out.push(' '); }
        out.push_str(line);
        if out.chars().count() > 550 { break; }
    }
    out.chars().take(550).collect()
}

/// A query must be explicitly prefixed with help/docs. Results are literal
/// excerpts from shipped documentation, not invented AI answers.
pub fn search(query: &str) -> Vec<Match> {
    let raw = query
        .strip_prefix("help ")
        .or_else(|| query.strip_prefix("docs "));
    let Some(raw) = raw else { return Vec::new() };
    let needle = raw.trim().to_lowercase();
    if !(2..=100).contains(&needle.chars().count()) || needle.chars().any(char::is_control) {
        return Vec::new();
    }
    let terms: Vec<_> = needle.split_whitespace().collect();
    let mut matches = Vec::new();
    for (source, markdown) in SOURCES {
        let lines: Vec<_> = markdown.lines().collect();
        let mut current_title = *source;
        let mut begin = 0;
        for i in 0..=lines.len() {
            let heading = if i < lines.len() && lines[i].trim().starts_with('#') {
                Some(lines[i].trim())
            } else { None };
            if heading.is_none() && i != lines.len() { continue; }
            if begin < i {
                let body = section_excerpt(&lines[begin..i]);
                let title = current_title.to_string();
                let haystack = format!("{} {}", title.to_lowercase(), body.to_lowercase());
                if terms.iter().all(|term| haystack.contains(term)) {
                    let score = if title.eq_ignore_ascii_case(&needle) { 0 }
                        else if title.to_lowercase().contains(&needle) { 1 }
                        else { 10 };
                    matches.push((score, Match { title, source: *source, excerpt: body }));
                }
            }
            if let Some(heading) = heading {
                current_title = heading.trim_start_matches('#').trim();
            }
            begin = i + 1;
        }
    }
    matches.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.title.cmp(&y.title)));
    matches.into_iter().take(5).map(|(_, hit)| hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_explicit_local_docs() {
        assert!(search("clipboard").is_empty());
        assert!(search("help ").is_empty());
        assert!(!search("help Keep Awake").is_empty());
        assert!(!search("docs vocabulary").is_empty());
        assert!(search("help something-that-does-not-exist-123").is_empty());
    }

    #[test]
    fn results_are_bounded_and_copied_from_embedded_sources() {
        let hits = search("help action");
        assert!(hits.len() <= 5);
        assert!(hits.iter().all(|hit| hit.excerpt.chars().count() <= 550));
        assert!(hits.iter().all(|hit| SOURCES.iter().any(|(name, _)| name == &hit.source)));
    }
}

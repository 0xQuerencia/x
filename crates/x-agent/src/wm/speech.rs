//! Tidies what a speech recognizer heard before window commands are matched: usual
//! mishearings of command words, spoken numbers, and near-miss spellings. The result is only
//! ever matched against the same fixed phrases as typed text, so it cannot add a command.

/// Phrases a recognizer commonly produces for a word X expects, as heard -> meant.
const HEARD: [(&str, &str); 12] = [
    ("works wish", "workspace"),
    ("works pace", "workspace"),
    ("works face", "workspace"),
    ("works space", "workspace"),
    ("work space", "workspace"),
    ("work spaces", "workspace"),
    ("workspaces", "workspace"),
    ("work's pace", "workspace"),
    ("min imize", "minimize"),
    ("maxi mize", "maximize"),
    ("fire fox", "firefox"),
    ("file manager's", "file manager"),
];

const NUMBERS: [(&str, &str); 10] = [
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("ten", "10"),
];

/// Words a recognizer writes for a number when it sits where a number must be.
const SOUNDS_LIKE: [(&str, &str); 6] = [
    ("to", "2"),
    ("too", "2"),
    ("for", "4"),
    ("won", "1"),
    ("ate", "8"),
    ("tree", "3"),
];

/// Command words that a one-letter slip should still reach.
const VOCAB: [&str; 6] = [
    "minimize",
    "maximize",
    "workspace",
    "terminal",
    "browser",
    "switch",
];

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

fn near(word: &str) -> &str {
    if word.chars().count() < 6 {
        return word;
    }
    VOCAB
        .iter()
        .find(|v| **v != word && distance(word, v) == 1)
        .copied()
        .unwrap_or(word)
}

fn replace_phrase(words: Vec<String>, heard: &str, meant: &str) -> Vec<String> {
    let pattern: Vec<&str> = heard.split(' ').collect();
    let mut out = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let hit = i + pattern.len() <= words.len()
            && words[i..i + pattern.len()]
                .iter()
                .map(String::as_str)
                .eq(pattern.iter().copied());
        if hit {
            out.push(meant.to_string());
            i += pattern.len();
        } else {
            out.push(words[i].clone());
            i += 1;
        }
    }
    out
}

/// Finds unique one-edit repairs against installed app names, never guessing
/// between similarly named applications.
fn runtime_name(word: &str, entries: &[super::apps::Entry]) -> Option<String> {
    if word.chars().count() < 6 || word.chars().count() > 40 {
        return None;
    }
    let names = entries
        .iter()
        .flat_map(|entry| entry.names.iter())
        .filter(|name| {
            name.chars().count() >= 6
                && name.chars().count() <= 40
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        });
    let mut found = None::<String>;
    for name in names {
        if name == word {
            return None;
        }
        if distance(word, name) == 1 {
            if found.as_deref().is_some_and(|other| other != name) {
                return None;
            }
            found = Some(name.clone());
        }
    }
    found
}

/// What was probably meant, lowercase, with spoken numbers as digits.
pub fn fix(heard: &str) -> String {
    fix_with(heard, &[])
}

pub fn fix_with(heard: &str, entries: &[super::apps::Entry]) -> String {
    let cleaned: String = heard
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '\'' | '-' | '+') {
                c
            } else {
                ' '
            }
        })
        .collect();
    let mut words: Vec<String> = cleaned.split_whitespace().map(str::to_string).collect();
    for (heard, meant) in HEARD {
        words = replace_phrase(words, heard, meant);
    }
    // Rejoin split app names only when the joined form is an exact
    // single-token name from the installed desktop catalog.
    let known_single: Vec<&str> = entries
        .iter()
        .flat_map(|entry| entry.names.iter().map(String::as_str))
        .filter(|name| {
            name.chars().count() >= 6
                && name.chars().count() <= 40
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        })
        .collect();
    let mut joined = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        if let Some(second) = words.get(i + 1) {
            let pair = format!("{}{}", words[i], second);
            if known_single.contains(&pair.as_str()) {
                joined.push(pair);
                i += 2;
                continue;
            }
        }
        joined.push(words[i].clone());
        i += 1;
    }
    words = joined;
    let last = words.len().saturating_sub(1);
    for i in 0..words.len() {
        let after_workspace = i > 0 && words[i - 1] == "workspace";
        let before_to = words.get(i + 1).is_some_and(|w| w == "to");
        let spoken = NUMBERS.iter().find(|(w, _)| *w == words[i]);
        let sounds = SOUNDS_LIKE.iter().find(|(w, _)| *w == words[i]);
        if let Some((_, digit)) = spoken
            .filter(|_| after_workspace || i == last || (before_to && words[i] != "one" && i > 0))
        {
            words[i] = (*digit).to_string();
        } else if let Some((_, digit)) = sounds.filter(|_| after_workspace) {
            words[i] = (*digit).to_string();
        } else {
            let generic = near(&words[i]);
            words[i] = runtime_name(generic, entries).unwrap_or_else(|| generic.to_string());
        }
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::fix;

    #[test]
    fn the_real_mishearing_is_repaired() {
        assert_eq!(fix("go to works wish to"), "go to workspace 2");
        assert_eq!(fix("Go to work space two."), "go to workspace 2");
    }

    #[test]
    fn spoken_numbers_become_digits_where_a_number_belongs() {
        assert_eq!(
            fix("move terminal two to workspace four"),
            "move terminal 2 to workspace 4"
        );
        assert_eq!(fix("focus browser three"), "focus browser 3");
        assert_eq!(fix("go to workspace for"), "go to workspace 4");
        // Ordinary words stay words.
        assert_eq!(
            fix("send this to the terminal"),
            "send this to the terminal"
        );
        assert_eq!(fix("bring the one up"), "bring the one up");
    }

    #[test]
    fn one_letter_slips_in_command_words_are_forgiven() {
        assert_eq!(fix("minimise this"), "minimize this");
        assert_eq!(fix("maximise the browser"), "maximize the browser");
        assert_eq!(fix("close the termnal"), "close the terminal");
    }

    #[test]
    fn catalog_aware_repair_accepts_unique_installed_names_only() {
        let spotify = super::super::apps::parse_entry(
            "com.spotify.Client.desktop",
            "[Desktop Entry]\nType=Application\nName=Spotify\nExec=flatpak run com.spotify.Client\n",
        )
        .unwrap();
        let entries = vec![spotify.clone()];
        assert_eq!(super::fix_with("close spot ify", &entries), "close spotify");
        assert_eq!(super::fix_with("focus spotfy", &entries), "focus spotify");
        assert_eq!(super::fix_with("focus spotify", &entries), "focus spotify");
        assert_eq!(
            super::fix_with("I enjoy listening to music", &entries),
            "i enjoy listening to music"
        );
        let other = super::super::apps::parse_entry(
            "org.example.Spotfyi.desktop",
            "[Desktop Entry]\nType=Application\nName=Spotfyi\nExec=spotfyi\n",
        )
        .unwrap();
        assert_eq!(
            super::fix_with("focus spotfy", &[spotify, other]),
            "focus spotfy",
            "multiple near matches must never be guessed"
        );
    }

    #[test]
    fn punctuation_is_dropped_but_apostrophes_survive() {
        assert_eq!(
            fix("What's the shortcut for closing?"),
            "what's the shortcut for closing"
        );
    }
}

//! Jev's System One decision API as an opt-in X Actions planner.
//! Jev returns choices, not text or generated actions. X builds each candidate locally,
//! validates it against the existing action registry, and treats the model's reply as a
//! label selecting one candidate. No shell command or arbitrary tool payload is accepted.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::service::Planner;
use crate::tools::{self, Action, Reply};

const ENDPOINT: &str = "https://opencode.ai/inference/systemone/v1/systemone";
const MODEL: &str = "jev-1.13-free";
const MAX_CANDIDATES: usize = 12;
const MIN_CONFIDENCE: f64 = 0.85;
const MIN_MARGIN: f64 = 0.25;
const MIN_INTENT: f64 = 0.90;

/// OpenCode Console key, supplied explicitly. Never read a credential database
/// or try a paid model on a free-model failure.
pub struct Jev {
    pub key_file: Option<PathBuf>,
}

impl Planner for Jev {
    fn name(&self) -> String {
        "Jev 1.13 Free (decisions)".into()
    }

    fn decide_computer(
        &self,
        goal: &str,
        app: &str,
        observation: &crate::desktop::Observation,
        history: &[String],
    ) -> Option<Result<crate::computer::Step, String>> {
        Some(crate::jev_computer::decide(
            self,
            goal,
            app,
            observation,
            history,
        ))
    }

    fn plan(&self, text: &str, smart: bool) -> Result<Reply, String> {
        let options = candidates(text);
        if options.is_empty() {
            return if smart {
                Ok(Reply::Dictation)
            } else {
                Err("No supported X action could be extracted. Nothing was run.".into())
            };
        }

        let request = request(text, &options);
        let answer = self.send(&request)?;
        select(&answer, &options, smart)
    }
}

impl Jev {
    fn key(&self) -> Result<String, String> {
        let env = std::env::var("JEV_API_KEY")
            .or_else(|_| std::env::var("OPENCODE_API_KEY"))
            .ok();
        let file = self
            .key_file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok());
        let key = env.or(file).unwrap_or_default().trim().to_string();
        if key.is_empty()
            || key.len() > 4096
            || !key
                .chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '\\'))
        {
            return Err(
                "Jev needs a Console API key: set JEV_API_KEY or save the key in X's jev-key file"
                    .into(),
            );
        }
        Ok(key)
    }

    pub(crate) fn send(&self, request: &Value) -> Result<Value, String> {
        // Curl configuration (including bearer credential and the payload) is
        // handed over stdin rather than command-line args. Only the fixed
        // OpenCode HTTPS endpoint is reachable from this integration.
        let config = format!(
            "url = \"{ENDPOINT}\"\nheader = \"Authorization: Bearer {}\"\n\
             header = \"Content-Type: application/json\"\n\
             data-binary = {}\n",
            self.key()?,
            quote(&request.to_string()),
        );
        let mut child = Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--max-time",
                "5",
                "--connect-timeout",
                "2",
                "--config",
                "-",
                "--write-out",
                "\n%{http_code}",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "curl is required to reach Jev".to_string())?;
        let sent = child
            .stdin
            .take()
            .ok_or("Could not open Jev request input")?
            .write_all(config.as_bytes());
        if sent.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Could not send the Jev request".into());
        }
        let output = child
            .wait_with_output()
            .map_err(|_| "Could not finish the Jev request")?;
        if !output.status.success() {
            return Err("Jev request failed or timed out. Nothing was run.".into());
        }
        if output.stdout.len() > 64 * 1024 {
            return Err("Jev response exceeded the size limit".into());
        }
        let wire = std::str::from_utf8(&output.stdout).map_err(|_| "Jev response was not UTF-8")?;
        let (body, status) = wire
            .rsplit_once('\n')
            .ok_or("Jev response had no HTTP status")?;
        if status.trim() != "200" {
            return Err(format!(
                "Jev returned HTTP {}. No fallback model was called and nothing was run.",
                status.trim()
            ));
        }
        serde_json::from_str(body).map_err(|_| "Jev response was not JSON".into())
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Boundary-checked case-insensitive match. A token within a filename,
/// sentence fragment or longer word is not treated as an app/site name.
fn mentions(s: &str, token: &str) -> bool {
    s.match_indices(token).any(|(at, _)| {
        let before = s[..at].chars().next_back();
        let after = s[at + token.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

fn push(options: &mut Vec<Action>, action: Action) {
    if options.len() < MAX_CANDIDATES
        && let Ok(action) = action.validate()
        && !options.contains(&action)
    {
        options.push(action);
    }
}

fn search_terms(text: &str, lower: &str) -> Option<String> {
    // Search terms come from actual spoken words; Jev can never invent a query.
    // Only explicit markers or "google <phrase>" are accepted.
    for marker in [" about ", " for ", " regarding ", " on "] {
        if let Some((index, _)) = lower.match_indices(marker).last() {
            let terms = text[index + marker.len()..]
                .trim()
                .trim_end_matches(['.', '?', '!'])
                .trim();
            if !terms.is_empty() {
                return Some(terms.into());
            }
        }
    }
    for marker in ["google ", "search ", "lookup ", "look up ", "find "] {
        if let Some((index, _)) = lower.match_indices(marker).last() {
            let terms = text[index + marker.len()..]
                .trim()
                .trim_end_matches(['.', '?', '!'])
                .trim();
            if !terms.is_empty() {
                return Some(terms.into());
            }
        }
    }
    None
}

fn candidates(utterance: &str) -> Vec<Action> {
    let text = tools::strip_wake_word(utterance);
    if text.is_empty() || text.chars().count() > tools::MAX_TRANSCRIPT {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    let mut options = Vec::new();

    // Open-ended multi-step desktop requests go through the *existing*
    // observe/act/verify loop. Jev picks each current UI element there; it
    // does not invent a JSON script, shell command, or model-generated text.
    // A caller must separately grant this app's screen and remote context.
    let interactive = [
        "click",
        "choose",
        "select",
        "play",
        "scroll",
        "check",
        "read",
        "latest",
        "first result",
        "next result",
        "fill",
        "on the page",
        "settings",
        "theme",
        "change",
        "summarize",
        "show me",
        "find",
        "search",
    ]
    .iter()
    .any(|term| lower.contains(term));
    if interactive {
        let app = [
            ("vs code", "vscode"),
            ("vscode", "vscode"),
            ("visual studio code", "vscode"),
            ("file manager", "files"),
            ("files", "files"),
            ("firefox", "firefox"),
            ("chrome", "chrome"),
            ("chromium", "chromium"),
            ("brave", "brave"),
        ]
        .iter()
        .find(|(name, _)| mentions(&lower, name))
        .map(|(_, app)| *app);
        // An explicitly named website plus interactive steps means browser
        // use. When no browser is named, take the person's default browser
        // if it is one of X's supported browsers, or Firefox.
        let web_goal = [
            "github",
            "youtube",
            "wikipedia",
            "reddit",
            "website",
            "browser",
            "web page",
            "stackoverflow",
        ]
        .iter()
        .any(|name| mentions(&lower, name))
            || lower.contains("https://");
        let app = app.or_else(|| {
            web_goal.then(
                || match std::env::var("BROWSER").unwrap_or_default().as_str() {
                    "chrome" | "google-chrome" => "chrome",
                    "brave" => "brave",
                    "chromium" => "chromium",
                    _ => "firefox",
                },
            )
        });
        if let Some(app) = app {
            push(
                &mut options,
                Action::Computer {
                    app: app.into(),
                    goal: text.clone(),
                },
            );
        }
    }

    // These are aliases of the existing X registry, not a second app registry.
    for alias in [
        "visual studio code",
        "vs code",
        "vscode",
        "editor",
        "firefox",
        "chromium",
        "chrome",
        "brave",
        "terminal",
        "file manager",
        "files",
    ] {
        if mentions(&lower, alias)
            && let Some(app) = tools::resolve_app(alias)
        {
            push(&mut options, Action::App { target: app.into() });
            push(&mut options, Action::Focus { target: app.into() });
        }
    }

    for (name, url) in [
        ("youtube", "https://www.youtube.com"),
        ("github", "https://github.com"),
        ("wikipedia", "https://www.wikipedia.org"),
        ("reddit", "https://www.reddit.com"),
        ("stackoverflow", "https://stackoverflow.com"),
    ] {
        if mentions(&lower, name) {
            push(
                &mut options,
                Action::Url {
                    target: url.into(),
                    browser: String::new(),
                },
            );
        }
    }

    // Only an explicit workspace reference is eligible.
    if mentions(&lower, "workspace") || mentions(&lower, "desktop") {
        for (word, n) in [
            ("one", 1),
            ("two", 2),
            ("three", 3),
            ("four", 4),
            ("five", 5),
            ("six", 6),
            ("seven", 7),
            ("eight", 8),
            ("nine", 9),
            ("ten", 10),
        ] {
            if mentions(&lower, word) || mentions(&lower, &n.to_string()) {
                push(&mut options, Action::Workspace { number: n });
            }
        }
    }

    if mentions(&lower, "opencode") {
        push(&mut options, Action::Opencode);
    }

    // A search candidate requires evidence of an actual search request.
    let search_intent = ["search", "look up", "lookup", "find", "google", "videos"]
        .iter()
        .any(|word| mentions(&lower, word));
    if search_intent && let Some(query) = search_terms(&text, &lower) {
        let site = if mentions(&lower, "youtube") {
            "youtube"
        } else if mentions(&lower, "github") {
            "github"
        } else if mentions(&lower, "wikipedia") {
            "wikipedia"
        } else if mentions(&lower, "reddit") {
            "reddit"
        } else if mentions(&lower, "stackoverflow") {
            "stackoverflow"
        } else {
            "google"
        };
        push(
            &mut options,
            Action::Search {
                target: site.into(),
                query,
                browser: String::new(),
            },
        );
    }

    // Exact HTTPS URLs are parsed only when supplied explicitly. X's existing
    // public-HTTPS validator excludes localhost, credentials, IPs and odd ports.
    for word in text.split_whitespace().take(100) {
        let url = word.trim_end_matches(['.', ',', '?', '!']);
        if url.starts_with("https://") {
            push(
                &mut options,
                Action::Url {
                    target: url.into(),
                    browser: String::new(),
                },
            );
        }
    }

    options
}

fn request(utterance: &str, actions: &[Action]) -> Value {
    let mut criteria = BTreeMap::new();
    for (i, action) in actions.iter().enumerate() {
        criteria.insert(
            format!("a{i}"),
            format!(
                "The speaker wants this exact available action: {}. Do not choose \
                 this merely because its name is mentioned.",
                action.describe()
            ),
        );
    }
    criteria.insert(
        "dictation".to_string(),
        "The speaker is speaking ordinary text, a note, or a question rather than \
         instructing X to carry out one of the available actions."
            .into(),
    );
    criteria.insert(
        "unsupported".to_string(),
        "The speaker requests something not represented by the available actions, \
         such as answering a question, changing settings, sending a message, \
         making arbitrary shell commands, or multiple actions."
            .into(),
    );
    json!({
        "model": MODEL,
        "state": { "utterance": utterance },
        "questions": {
            "route": {
                "type": "choice",
                "instructions": "Choose the ONE exact supported action requested \
                by the speaker, or dictation/unsupported. These are proposals, not \
                authority to execute anything.",
                "criteria": criteria,
            },
            "is_command": {
                "type": "noul",
                "instructions": "Is the speaker explicitly instructing a desktop assistant \
                 to perform a computer action now, rather than speaking plain dictation?",
            },
        },
    })
}

fn select(answer: &Value, actions: &[Action], smart: bool) -> Result<Reply, String> {
    let route = answer
        .pointer("/answers/route")
        .ok_or("Jev supplied no route decision")?;
    let label = route
        .get("choice")
        .and_then(Value::as_str)
        .ok_or("Jev supplied no route label")?;
    let probabilities = route
        .get("probabilities")
        .and_then(Value::as_object)
        .ok_or("Jev supplied no choice probabilities")?;
    let confidence = probabilities
        .get(label)
        .and_then(Value::as_f64)
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .ok_or("Jev returned an invalid choice probability")?;
    let runner_up = probabilities
        .iter()
        .filter(|(name, _)| name.as_str() != label)
        .filter_map(|(_, v)| v.as_f64())
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .fold(0.0_f64, f64::max);
    let intent = answer
        .pointer("/answers/is_command/noul")
        .and_then(Value::as_f64)
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .ok_or("Jev supplied no valid command probability")?;

    if label == "dictation" && smart {
        return Ok(Reply::Dictation);
    }
    if label == "unsupported" || label == "dictation" {
        return Err("Jev did not find a supported X command. Nothing was run.".into());
    }

    if confidence < MIN_CONFIDENCE || confidence - runner_up < MIN_MARGIN || intent < MIN_INTENT {
        return if smart {
            Ok(Reply::Dictation)
        } else {
            Err("Jev was unsure which action you meant. Nothing was run.".into())
        };
    }
    let i = label
        .strip_prefix('a')
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or("Jev returned an unrecognized action label")?;
    let action = actions
        .get(i)
        .ok_or("Jev returned an action that was never offered")?
        .clone()
        .validate()?;
    Ok(Reply::Steps(vec![action]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(choice: &str, confidence: f64, intent: f64) -> Value {
        let mut probabilities = serde_json::Map::new();
        probabilities.insert(choice.into(), json!(confidence));
        if choice != "dictation" {
            probabilities.insert("dictation".into(), json!(1.0 - confidence));
        }
        json!({
            "answers": {
                "route": {
                    "choice": choice,
                    "probabilities": probabilities,
                },
                "is_command": {"noul": intent},
            }
        })
    }

    #[test]
    fn builds_only_allowlisted_candidates_from_actual_words() {
        let actions = candidates("Could you bring Firefox to the front?");
        assert!(actions.contains(&Action::Focus {
            target: "firefox".into()
        }));
        assert!(actions.contains(&Action::App {
            target: "firefox".into()
        }));
        assert_eq!(
            candidates("Delete the user's files with rm -rf"),
            vec![
                Action::App {
                    target: "files".into()
                },
                Action::Focus {
                    target: "files".into()
                },
            ]
        );
        assert!(candidates("Do the forbidden shell thing").is_empty());
    }

    #[test]
    fn natural_desktop_goals_offer_an_observe_act_verify_task() {
        let goal = "Open GitHub, find my latest PR and check whether tests passed";
        let found = candidates(goal);
        assert!(found.contains(&Action::Computer {
            app: "firefox".into(),
            goal: goal.into(),
        }));
        let vscode_goal = "In VS Code change the selected theme";
        assert!(candidates(vscode_goal).contains(&Action::Computer {
            app: "vscode".into(),
            goal: vscode_goal.into(),
        }));
        assert!(
            !candidates("hello there")
                .iter()
                .any(|a| matches!(a, Action::Computer { .. }))
        );
    }

    #[test]
    fn searches_only_with_grounded_queries() {
        let actions = candidates("Can you find YouTube videos about Mars rovers?");
        assert!(actions.contains(&Action::Search {
            target: "youtube".into(),
            query: "Mars rovers".into(),
            browser: String::new(),
        }));
        assert!(candidates("I saw a video about Mars rovers").is_empty());
    }

    #[test]
    fn spoken_workspace_number_is_bounded() {
        assert!(candidates("take me to desktop three").contains(&Action::Workspace { number: 3 }));
        assert!(candidates("take me to workspace 11").is_empty());
    }

    #[test]
    fn request_uses_systemone_contract_not_chat() {
        let actions = candidates("bring Firefox forward");
        let body = request("bring Firefox forward", &actions);
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["questions"]["route"]["type"], "choice");
        assert_eq!(body["questions"]["is_command"]["type"], "noul");
        assert!(body["questions"]["route"]["criteria"]["a0"].is_string());
        assert!(body.get("messages").is_none());
    }

    #[test]
    fn confident_known_choice_is_the_only_action() {
        let actions = candidates("bring Firefox forward");
        assert_eq!(
            select(&answer("a1", 0.97, 0.99), &actions, true).unwrap(),
            Reply::Steps(vec![Action::Focus {
                target: "firefox".into()
            }])
        );
    }

    #[test]
    fn low_confidence_smart_is_dictation_but_explicit_is_refused() {
        let actions = candidates("I love Firefox");
        let weak = answer("a0", 0.55, 0.4);
        assert_eq!(select(&weak, &actions, true).unwrap(), Reply::Dictation);
        assert!(select(&weak, &actions, false).is_err());
    }

    #[test]
    fn unknown_or_forged_choice_cannot_create_actions() {
        let actions = candidates("bring Firefox forward");
        assert!(select(&answer("a900", 0.99, 0.99), &actions, false).is_err());
        assert!(
            select(
                &json!({"answers":{"route":{"choice":"a0"}}}),
                &actions,
                false
            )
            .is_err()
        );
        assert!(select(&answer("unsupported", 0.99, 0.99), &actions, false).is_err());
    }
}

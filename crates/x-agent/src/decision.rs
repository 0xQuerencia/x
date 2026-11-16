//! Shared choice router for X's Perplexity Decider planners.
//! The model returns choices, not text or generated actions. X builds each candidate locally,
//! validates it against the existing action registry, and treats the model's reply as a
//! label selecting one candidate. No shell command or arbitrary tool payload is accepted.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::tools::{self, Action, Reply};

const MAX_CANDIDATES: usize = 12;
const MIN_CONFIDENCE: f64 = 0.85;
const MIN_MARGIN: f64 = 0.25;
const MIN_INTENT: f64 = 0.90;

/// Shared, non-generative choice router used by the free Decider models.
pub(crate) fn plan_with(
    send: &dyn Fn(&Value) -> Result<Value, String>,
    model: &str,
    text: &str,
    smart: bool,
) -> Result<Reply, String> {
    let options = candidates(text);
    if options.is_empty() {
        return if smart {
            Ok(Reply::Dictation)
        } else {
            Err("No supported X action could be extracted. Nothing was run.".into())
        };
    }

    let mut request = request(text, &options);
    request["model"] = json!(model);
    let answer = send(&request)?;
    select(&answer, &options, smart)
}

/// Small local-only circuit breaker: bad network conditions do not stall every
/// subsequent voice command. Exact local rules bypass this path entirely.
struct Circuit {
    consecutive_failures: u8,
    until: Option<Instant>,
}

impl Circuit {
    const fn new() -> Self {
        Self {
            consecutive_failures: 0,
            until: None,
        }
    }

    fn before(&mut self) -> Result<(), String> {
        if let Some(until) = self.until
            && let Some(remaining) = until.checked_duration_since(Instant::now())
        {
            return Err(format!(
                "Remote choice is cooling down (about {}s). Exact local commands still work.",
                remaining.as_secs().max(1)
            ));
        }
        self.until = None;
        Ok(())
    }

    fn success(&mut self) {
        self.consecutive_failures = 0;
        self.until = None;
    }

    fn failed(&mut self, retry_after: Option<Duration>) {
        if let Some(wait) = retry_after {
            self.until = Some(Instant::now() + wait);
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            if self.consecutive_failures >= 2 {
                self.until = Some(Instant::now() + Duration::from_secs(40));
                self.consecutive_failures = 0;
            }
        }
    }
}

static CIRCUIT: Mutex<Circuit> = Mutex::new(Circuit::new());

enum NetworkFailure {
    Timeout,
    Offline,
    Other,
}

fn transport(config: &str) -> Result<String, NetworkFailure> {
    let mut child = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "5",
            "--connect-timeout",
            "1",
            "--dump-header",
            "-",
            "--config",
            "-",
            "--write-out",
            "\n%{http_code}",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| NetworkFailure::Offline)?;
    if child
        .stdin
        .take()
        .ok_or(NetworkFailure::Other)?
        .write_all(config.as_bytes())
        .is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(NetworkFailure::Offline);
    }
    let output = child
        .wait_with_output()
        .map_err(|_| NetworkFailure::Other)?;
    if !output.status.success() {
        return Err(match output.status.code() {
            Some(28) => NetworkFailure::Timeout,
            Some(5 | 6 | 7 | 35 | 52 | 56) => NetworkFailure::Offline,
            _ => NetworkFailure::Other,
        });
    }
    if output.stdout.len() > 64 * 1024 {
        return Err(NetworkFailure::Other);
    }
    String::from_utf8(output.stdout).map_err(|_| NetworkFailure::Other)
}

struct Response<'a> {
    status: u16,
    body: &'a str,
    retry_after: Option<Duration>,
}

fn response(wire: &str) -> Result<Response<'_>, String> {
    let (payload, status) = wire
        .rsplit_once('\n')
        .ok_or("Decision response had no HTTP status")?;
    let status = status
        .trim()
        .parse::<u16>()
        .map_err(|_| "Decision response had an invalid HTTP status")?;
    // curl -D - writes the response headers to the same stdout stream,
    // including all 1xx/proxy headers. Only the final header block matters.
    let (headers, body) = payload
        .rsplit_once("\r\n\r\n")
        .or_else(|| payload.rsplit_once("\n\n"))
        .ok_or("Decision response had no HTTP headers")?;
    let retry_after = headers
        .lines()
        .rev()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("retry-after")
                .then(|| value.trim().parse::<u64>().ok())
                .flatten()
        })
        .map(|seconds| Duration::from_secs(seconds.clamp(1, 120)));
    Ok(Response {
        status,
        body,
        retry_after,
    })
}

/// Fixed-endpoint Decisions API transport; credentials and payload never reach argv.
pub(crate) fn send_with(endpoint: &str, key: &str, request: &Value) -> Result<Value, String> {
    {
        let mut circuit = CIRCUIT.lock().unwrap_or_else(|p| p.into_inner());
        circuit.before()?;
    }
    let config = format!(
        "url = \"{endpoint}\"\nheader = \"Authorization: Bearer {}\"\n\
         header = \"Content-Type: application/json\"\n\
         data-binary = {}\n",
        key,
        quote(&request.to_string()),
    );
    let wire = match transport(&config) {
        Ok(wire) => wire,
        Err(kind) => match kind {
            NetworkFailure::Offline | NetworkFailure::Timeout => {
                CIRCUIT
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .failed(None);
                return Err(
                    "Remote choice is offline or timed out; exact commands still work.".into(),
                );
            }
            NetworkFailure::Other => {
                return Err("Decision request failed; no action was run.".into());
            }
        },
    };
    let reply = response(&wire)?;
    match reply.status {
        200 => {
            CIRCUIT.lock().unwrap_or_else(|p| p.into_inner()).success();
            serde_json::from_str(reply.body).map_err(|_| "Decision response was not JSON".into())
        }
        429 => {
            CIRCUIT
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .failed(Some(reply.retry_after.unwrap_or(Duration::from_secs(40))));
            Err(
                "Remote choice is rate limited. Exact local commands still work; nothing was run."
                    .into(),
            )
        }
        401 | 403 => {
            Err("Decision provider denied authentication or access. No action was run.".into())
        }
        500..=599 => Err("Decision provider is unavailable. No fallback was called.".into()),
        other => Err(format!(
            "Decision provider returned HTTP {other}. No fallback model was called."
        )),
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
    // Search terms come from actual spoken words; the model can never invent a query.
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

/// Turn loosely phrased desktop verbs into *typed* window operations.
/// This is a pure candidate builder: it does not run a command or trust
/// remote-generated target IDs. The controller resolves the window freshly.
fn window_offers(text: &str, entries: &[crate::wm::apps::Entry]) -> Vec<Action> {
    use crate::wm::{Op, Target, Ws};
    let lower = text.trim().to_lowercase();
    let mut said = lower.trim_end_matches(['.', '?', '!', ',']).trim();
    for prefix in ["please ", "could you ", "can you ", "would you ", "hey x "] {
        if let Some(rest) = said.strip_prefix(prefix) {
            said = rest.trim();
            break;
        }
    }

    // Require an imperative at the *beginning* of the utterance. Merely
    // mentioning a destructive verb in prose must never offer a close.
    let patterns: &[(&str, Op)] = &[
        ("get rid of ", Op::Close),
        ("shut down ", Op::Close),
        ("close ", Op::Close),
        ("quit ", Op::Close),
        ("exit ", Op::Close),
        ("shut ", Op::Close),
        ("hide ", Op::Minimize),
        ("tuck ", Op::Minimize),
        ("minimize ", Op::Minimize),
        ("maximise ", Op::Maximize),
        ("maximize ", Op::Maximize),
        ("fullscreen ", Op::Maximize),
        ("bring ", Op::Focus),
        ("show ", Op::Focus),
        ("focus ", Op::Focus),
        ("switch to ", Op::Focus),
        ("put ", Op::Move),
        ("move ", Op::Move),
        ("send ", Op::Move),
    ];
    let Some((op, mut who)) = patterns
        .iter()
        .find_map(|(p, op)| said.strip_prefix(p).map(|rest| ((*op).clone(), rest)))
    else {
        return Vec::new();
    };

    let ws = if op == Op::Move {
        let Some((subject, destination)) = who
            .split_once(" to workspace ")
            .or_else(|| who.split_once(" on workspace "))
            .or_else(|| who.split_once(" to desktop "))
            .or_else(|| who.split_once(" on desktop "))
        else {
            return Vec::new();
        };
        let number = match destination.trim() {
            "one" => 1,
            "two" => 2,
            "three" => 3,
            "four" => 4,
            "five" => 5,
            "six" => 6,
            "seven" => 7,
            "eight" => 8,
            "nine" => 9,
            "ten" => 10,
            other => match other.parse::<u8>() {
                Ok(number) if (1..=10).contains(&number) => number,
                _ => return Vec::new(),
            },
        };
        who = subject;
        Some(Ws::N(number))
    } else {
        None
    };

    if op == Op::Focus {
        for suffix in [" to the front", " to front", " forward", " up"] {
            if let Some(rest) = who.strip_suffix(suffix) {
                who = rest;
                break;
            }
        }
    }
    if op == Op::Minimize
        && let Some(rest) = who.strip_suffix(" away")
    {
        who = rest;
    }

    let target = if matches!(
        who.trim(),
        "this" | "it" | "this window" | "this app" | "the current window" | "the app"
    ) {
        Target::default()
    } else {
        let name = crate::wm::apps::normalize(who);
        if name.is_empty() || name.len() > 64 {
            return Vec::new();
        }
        // An installed app (or a known built-in alias until P1-5 replaces it)
        // is grounding for a window target; arbitrary spoken titles are not.
        if !crate::wm::apps::known(&name, entries) && tools::resolve_app(&name).is_none() {
            return Vec::new();
        }
        Target {
            app: name,
            pick: None,
        }
    };
    let action = Action::Window { op, target, ws };
    action.validate().ok().into_iter().collect()
}

fn candidates(utterance: &str) -> Vec<Action> {
    let text = tools::strip_wake_word(utterance);
    if text.is_empty() || text.chars().count() > tools::MAX_TRANSCRIPT {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    let mut options = Vec::new();
    let window = window_offers(&text, &crate::wm::apps::load());
    for action in &window {
        push(&mut options, action.clone());
    }

    // Open-ended multi-step desktop requests go through the *existing*
    // observe/act/verify loop. the model picks each current UI element there; it
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

    // A window imperative must never compete with an unrelated launch or
    // legacy focus candidate. Otherwise the decision model could select
    // "open" when the person explicitly asked to "close".
    if window.is_empty() {
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
        "model": crate::decider::Model::Flash.id(),
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
                "criteria": {
                    "true": "An explicit instruction to perform a supported computer action now",
                    "false": "Plain dictation, a question, or another unsupported goal",
                },
            },
        },
    })
}

fn select(answer: &Value, actions: &[Action], smart: bool) -> Result<Reply, String> {
    let route = answer
        .pointer("/answers/route")
        .ok_or("The decision model supplied no route decision")?;
    let label = route
        .get("choice")
        .and_then(Value::as_str)
        .ok_or("The decision model supplied no route label")?;
    let probabilities = route
        .get("probabilities")
        .and_then(Value::as_object)
        .ok_or("The decision model supplied no choice probabilities")?;
    let confidence = probabilities
        .get(label)
        .and_then(Value::as_f64)
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .ok_or("The decision model returned an invalid choice probability")?;
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
        .ok_or("The decision model supplied no valid command probability")?;

    if label == "dictation" && smart {
        return Ok(Reply::Dictation);
    }
    if label == "unsupported" || label == "dictation" {
        return Err(
            "The decision model did not find a supported X command. Nothing was run.".into(),
        );
    }

    if confidence < MIN_CONFIDENCE || confidence - runner_up < MIN_MARGIN || intent < MIN_INTENT {
        return if smart {
            Ok(Reply::Dictation)
        } else {
            Err("The decision model was unsure which action you meant. Nothing was run.".into())
        };
    }
    let i = label
        .strip_prefix('a')
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or("The decision model returned an unrecognized action label")?;
    let action = actions
        .get(i)
        .ok_or("The decision model returned an action that was never offered")?
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
    fn decider_transport_parses_rate_limit_without_leaking_body() {
        let wire = "HTTP/2 429\r\nContent-Type: application/json\r\nRetry-After: 12\r\n\r\n{\"error\":\"private\"}\n429";
        let result = response(wire).unwrap();
        assert_eq!(result.status, 429);
        assert_eq!(result.retry_after, Some(Duration::from_secs(12)));
        let mut circuit = Circuit::new();
        circuit.failed(result.retry_after);
        assert!(circuit.before().unwrap_err().contains("cooling down"));
    }

    #[test]
    fn two_network_failures_trip_a_bounded_circuit_breaker() {
        let mut circuit = Circuit::new();
        assert!(circuit.before().is_ok());
        circuit.failed(None);
        assert!(circuit.before().is_ok());
        circuit.failed(None);
        assert!(circuit.before().is_err());
        circuit.success();
        assert!(circuit.before().is_ok());
    }

    #[test]
    fn successful_response_body_survives_proxy_headers() {
        let wire = concat!(
            "HTTP/1.1 200 Connection established\r\n\r\n",
            "HTTP/2 200\r\nContent-Type: application/json\r\n\r\n",
            "{\"answers\":{\"route\":{}}}\n200"
        );
        let result = response(wire).unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(result.body, "{\"answers\":{\"route\":{}}}");
    }

    #[test]
    fn loose_window_commands_offer_typed_desktop_actions_without_planner_json() {
        use crate::wm::{Op, Ws, apps};
        let catalog = vec![
            apps::parse_entry(
                "com.spotify.Client.desktop",
                "[Desktop Entry]\nType=Application\nName=Spotify\nExec=flatpak run com.spotify.Client\nStartupWMClass=spotify\n",
            )
            .unwrap(),
            apps::parse_entry(
                "foot.desktop",
                "[Desktop Entry]\nType=Application\nName=Foot\nExec=foot\nCategories=TerminalEmulator;\n",
            )
            .unwrap(),
        ];
        for (spoken, expected) in [
            ("get rid of the terminal", Op::Close),
            ("please hide spotify", Op::Minimize),
            ("tuck the terminal away", Op::Minimize),
            ("could you bring Spotify to the front", Op::Focus),
            ("fullscreen Spotify", Op::Maximize),
            ("put Spotify on workspace three", Op::Move),
        ] {
            let actions = window_offers(spoken, &catalog);
            assert_eq!(actions.len(), 1, "{spoken}");
            let Action::Window { op, .. } = &actions[0] else {
                panic!("Expected a window action for {spoken}");
            };
            assert_eq!(*op, expected, "{spoken}");
        }
        assert!(
            window_offers("put Spotify on workspace three", &catalog)
                .iter()
                .any(|action| matches!(
                    action,
                    Action::Window {
                        ws: Some(Ws::N(3)),
                        ..
                    }
                ))
        );
        for said in [
            "I heard someone say close Spotify",
            "the article discussed closing a terminal",
            "shut the universe",
            "put Spotify on workspace 99",
            "close this tab",
        ] {
            assert!(window_offers(said, &catalog).is_empty(), "{said}");
        }
    }

    #[test]
    fn builds_only_allowlisted_candidates_from_actual_words() {
        let actions = candidates("Could you bring Firefox to the front?");
        assert!(actions.contains(&Action::Window {
            op: crate::wm::Op::Focus,
            target: crate::wm::Target {
                app: "firefox".into(),
                pick: None,
            },
            ws: None,
        }));
        assert!(
            !actions.iter().any(|action| matches!(action, Action::App { .. })),
            "a focus command must not be reinterpreted as launching another app"
        );
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
    fn request_uses_decisions_contract_not_chat() {
        let actions = candidates("bring Firefox forward");
        let body = request("bring Firefox forward", &actions);
        assert_eq!(body["model"], crate::decider::Model::Flash.id());
        assert_eq!(body["questions"]["route"]["type"], "choice");
        assert_eq!(body["questions"]["is_command"]["type"], "noul");
        assert!(body["questions"]["route"]["criteria"]["a0"].is_string());
        assert!(body.get("messages").is_none());
    }

    #[test]
    fn confident_known_choice_is_the_only_action() {
        let actions = candidates("bring Firefox forward");
        assert_eq!(
            select(&answer("a0", 0.97, 0.99), &actions, true).unwrap(),
            Reply::Steps(vec![Action::Window {
                op: crate::wm::Op::Focus,
                target: crate::wm::Target {
                    app: "firefox".into(),
                    pick: None,
                },
                ws: None,
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

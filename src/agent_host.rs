//! X, the agent layer: the one task service in this Hex process and the `hex agent`
//! control commands that drive it. The logic lives in `crates/x-agent`; this file only
//! connects it to Hex's Labs switch and control protocol.
//!
//! Speech goes through Hex's own control CLI (`dictation start --raw` / `stop --print`),
//! so a transcript never pastes into the focused app.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use x_agent::hexcli::HexCli;
use x_agent::service::{Mode, RealLauncher, Service};

use crate::control::{AgentAction, ControlReply};

const DISABLED: &str = "X is off. Turn it on with `hex labs on x-agent`";

/// OpenCode's installer puts the real binary in `~/.opencode/bin`, which shell aliases
/// and wrappers on PATH can shadow, so look there too.
fn opencode_exe() -> Option<PathBuf> {
    x_agent::tools::which("opencode")
        .map(PathBuf::from)
        .or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".opencode/bin/opencode"))
                .filter(|p| p.is_file())
        })
}

pub fn service() -> &'static Service {
    static SERVICE: OnceLock<Service> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let exe = std::env::current_exe().map_or_else(
            |_| "hex".into(),
            |path| {
                path.display()
                    .to_string()
                    .trim_end_matches(" (deleted)")
                    .to_string()
            },
        );
        let service = Service::new(
            Arc::new(HexCli { exe }),
            Arc::new(RealLauncher),
            opencode_exe(),
        );
        service.set_trust_actions(trust_file().is_some_and(|path| path.is_file()));
        service.set_computer_apps(load_computer_apps());
        service.set_computer_remote(computer_remote_file().is_some_and(|path| path.is_file()));
        VOICE.store(load_voice_mode() as u8, Ordering::Relaxed);
        x_agent::tools::set_wake_words(load_wake_words());
        let saved = planner_file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        service.set_planner(saved.map(planner_for));
        let watched = service.clone();
        thread::spawn(move || watch(&watched));
        service
    })
}

/// Present when the person opted into trusted Actions. It lives beside the other trust
/// files in the data folder, so nothing in a config can switch it on.
fn trust_file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("x-trusted-actions"))
}

fn planner_file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("x-planner"))
}

fn gemini_key_file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("gemini-key"))
}

/// `gemini` or `gemini:<model>` is Gemini; any other name is a local Ollama model.
fn planner_for(model: String) -> Arc<dyn x_agent::service::Planner> {
    if let Some(name) = model
        .strip_prefix("openai:")
        .filter(|name| !name.is_empty())
    {
        let dir = crate::app_paths::support_dir().unwrap_or_default();
        return Arc::new(x_agent::planner::OpenAi {
            model: name.to_string(),
            base_file: dir.join("openai-base"),
            key_file: dir.join("openai-key"),
        });
    }
    match model.strip_prefix("gemini") {
        Some(rest) if rest.is_empty() || rest.starts_with(':') => {
            Arc::new(x_agent::planner::Gemini {
                model: rest
                    .strip_prefix(':')
                    .filter(|m| !m.is_empty())
                    .unwrap_or(x_agent::planner::GEMINI_DEFAULT)
                    .to_string(),
                key_file: gemini_key_file(),
            })
        }
        _ => Arc::new(x_agent::planner::Ollama { model }),
    }
}

fn set_planner(model: &str) -> ControlReply {
    let model = model.trim();
    if model.is_empty() || model.len() > 100 || model.contains(char::is_whitespace) {
        return ControlReply::error("usage", "give an Ollama model name, or off");
    }
    let Some(path) = planner_file() else {
        return ControlReply::error("failed", "the data folder is unavailable");
    };
    let off = model == "off";
    let saved = if off {
        std::fs::remove_file(&path).or_else(ignore_missing)
    } else {
        write_private(&path, model.as_bytes())
    };
    if let Err(error) = saved {
        return ControlReply::error("failed", format!("couldn't save the setting: {error}"));
    }
    service().set_planner((!off).then(|| planner_for(model.to_string())));
    if off {
        return ControlReply::done("Planner off");
    }
    let note = if model.starts_with("openai:") {
        ". What you say is sent to the endpoint saved in X's openai-base file for planning."
            .to_string()
    } else if model.starts_with("gemini") {
        let has_key = std::env::var_os("GEMINI_API_KEY").is_some()
            || gemini_key_file().is_some_and(|f| f.is_file());
        format!(
            ". What you say is sent to Google for planning.{}",
            if has_key {
                ""
            } else {
                " No key yet: put it in the gemini-key file in X's data folder, or set GEMINI_API_KEY"
            }
        )
    } else {
        String::new()
    };
    ControlReply::done(format!("Planner is {model}{note}"))
}

fn wake_words_file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("x-wake-words"))
}

fn load_wake_words() -> Vec<String> {
    wake_words_file()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| x_agent::tools::valid_wake_word(line).ok())
        .collect()
}

fn wake_summary() -> String {
    format!("Wake words: {}", x_agent::tools::wake_words().join(", "))
}

/// `list`, `add <word>`, `remove <word>`, `set <word>`. Any single word works; at least one
/// always stays so a spoken command is never impossible.
fn wake_words_command(command: &str, value: &str) -> ControlReply {
    let mut words = x_agent::tools::wake_words();
    match command {
        "list" => return ControlReply::done(wake_summary()),
        "add" | "set" => match x_agent::tools::valid_wake_word(value) {
            Ok(word) if command == "set" => words = vec![word],
            Ok(word) => {
                if !words.contains(&word) {
                    words.push(word);
                }
            }
            Err(error) => return ControlReply::error("usage", error),
        },
        "remove" => {
            let word = value.trim().to_lowercase();
            if !words.contains(&word) {
                return ControlReply::error("usage", format!("{word:?} is not a wake word"));
            }
            if words.len() == 1 {
                return ControlReply::error("refused", "keep at least one wake word");
            }
            words.retain(|w| *w != word);
        }
        _ => return ControlReply::error("usage", "list, add <word>, remove <word> or set <word>"),
    }
    let Some(path) = wake_words_file() else {
        return ControlReply::error("failed", "the data folder is unavailable");
    };
    if let Err(error) = write_private(&path, words.join("\n").as_bytes()) {
        return ControlReply::error("failed", format!("couldn't save the setting: {error}"));
    }
    x_agent::tools::set_wake_words(words);
    ControlReply::done(wake_summary())
}

fn private_file(path: &std::path::Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn write_private(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    std::io::Write::write_all(&mut private_file(path, false)?, contents)
}

fn append_private(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    std::io::Write::write_all(&mut private_file(path, true)?, contents)
}

fn computer_apps_file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("x-computer-apps"))
}

fn computer_remote_file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("x-computer-remote"))
}

fn approval_audit_file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("x-approval-audit.ndjson"))
}

fn load_computer_apps() -> Vec<String> {
    computer_apps_file()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|app| x_agent::tools::APPS.iter().any(|(name, _)| name == app))
        .map(String::from)
        .collect()
}

fn computer_summary(apps: &[String]) -> String {
    let remote = computer_remote_file().is_some_and(|path| path.is_file());
    format!(
        "Screen access: {}. On-screen text may go to remote models: {}",
        if apps.is_empty() {
            "none granted".to_string()
        } else {
            apps.join(", ")
        },
        if remote { "yes" } else { "no" }
    )
}

/// Grants are files in X's data folder: only the person's own command changes them, never
/// a model, a plan or anything read from the screen.
fn computer_access(command: &str, value: &str) -> ControlReply {
    let (Some(apps_path), Some(remote_path)) = (computer_apps_file(), computer_remote_file())
    else {
        return ControlReply::error("failed", "the data folder is unavailable");
    };
    let mut apps = load_computer_apps();
    let result = match command {
        "list" => return ControlReply::done(computer_summary(&apps)),
        "remote" if matches!(value, "on" | "off") => {
            let saved = if value == "on" {
                write_private(&remote_path, b"1")
            } else {
                std::fs::remove_file(&remote_path).or_else(ignore_missing)
            };
            service().set_computer_remote(value == "on");
            saved
        }
        "grant" | "revoke" => {
            if !x_agent::tools::APPS.iter().any(|(name, _)| *name == value) {
                return ControlReply::error("usage", "give an app such as chrome or firefox");
            }
            apps.retain(|app| app != value);
            if command == "grant" {
                apps.push(value.to_string());
            }
            service().set_computer_apps(apps.clone());
            write_private(&apps_path, apps.join("\n").as_bytes())
        }
        _ => return ControlReply::error("usage", "grant|revoke <app>, list, or remote on|off"),
    };
    match result {
        Ok(()) => ControlReply::done(computer_summary(&load_computer_apps())),
        Err(error) => ControlReply::error("failed", format!("couldn't save the setting: {error}")),
    }
}

fn ignore_missing(error: std::io::Error) -> std::io::Result<()> {
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(error)
    }
}

fn set_trust(on: bool) -> ControlReply {
    let Some(path) = trust_file() else {
        return ControlReply::error("failed", "the data folder is unavailable");
    };
    let saved = if on {
        write_private(&path, b"1")
    } else {
        std::fs::remove_file(&path).or_else(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(error),
        })
    };
    match saved {
        Ok(()) => {
            service().set_trust_actions(on);
            ControlReply::done(if on {
                "Trusted Actions on"
            } else {
                "Trusted Actions off"
            })
        }
        Err(error) => ControlReply::error("failed", format!("couldn't save the setting: {error}")),
    }
}

/// What the dictation shortcut's transcript becomes. `Dictate` is Hex as it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceMode {
    Dictate = 0,
    Actions = 1,
    Work = 2,
    Auto = 3,
    Smart = 4,
}

impl VoiceMode {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "dictate" => Some(Self::Dictate),
            "actions" => Some(Self::Actions),
            "work" => Some(Self::Work),
            "auto" => Some(Self::Auto),
            "smart" => Some(Self::Smart),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Dictate => "dictate",
            Self::Actions => "actions",
            Self::Work => "work",
            Self::Auto => "auto",
            Self::Smart => "smart",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Actions,
            2 => Self::Work,
            3 => Self::Auto,
            4 => Self::Smart,
            _ => Self::Dictate,
        }
    }
}

static VOICE: AtomicU8 = AtomicU8::new(0);

fn voice_file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("x-voice-mode"))
}

fn load_voice_mode() -> VoiceMode {
    voice_file()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| VoiceMode::parse(text.trim()))
        .unwrap_or(VoiceMode::Dictate)
}

pub fn voice_mode() -> VoiceMode {
    service();
    VoiceMode::from_u8(VOICE.load(Ordering::Relaxed))
}

pub fn set_voice_mode(mode: VoiceMode) -> ControlReply {
    let Some(path) = voice_file() else {
        return ControlReply::error("failed", "the data folder is unavailable");
    };
    if let Err(error) = write_private(&path, mode.name().as_bytes()) {
        return ControlReply::error("failed", format!("couldn't save the mode: {error}"));
    }
    service();
    VOICE.store(mode as u8, Ordering::Relaxed);
    ControlReply::done(format!("Voice mode is {}", mode.name()))
}

/// In `Auto`, speech runs as a command only after a spoken wake word (see `agent wake`); the rest
/// types as dictation. "work" right after the prefix hands the goal to OpenCode.
fn spoken_command(text: &str) -> Option<(Mode, String)> {
    let mut words = text.split_whitespace();
    let clean = |word: &str| {
        word.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let mut first = clean(words.next()?);
    if matches!(first.as_str(), "hey" | "hi" | "okay" | "ok") {
        first = clean(words.next()?);
    }
    if !x_agent::tools::wake_words().contains(&first) {
        return None;
    }
    let mut rest: Vec<&str> = words.collect();
    let mode = if rest.first().is_some_and(|word| clean(word) == "work") {
        rest.remove(0);
        Mode::Work
    } else {
        Mode::Actions
    };
    let command = rest.join(" ");
    let command = command.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
    (!command.is_empty()).then(|| (mode, command.to_string()))
}

/// Called after dictation was pasted, so "make that shorter" and "scratch that" know what
/// "that" is. Remembers the text and window in memory only (bounded, expiring); it reads
/// nothing from the app. Does nothing when X is off.
pub fn record_insertion(text: &str) {
    if crate::labs::x_agent() {
        service().record_insertion(text);
    }
}

/// Called with a finished dictation before it is pasted: true when X took it, so it is
/// not typed. Dictate mode, X off, and an Auto transcript without the prefix return false.
pub fn intercept(text: &str) -> bool {
    if !crate::labs::x_agent() {
        return false;
    }
    let (mode, command) = match voice_mode() {
        VoiceMode::Dictate => return false,
        VoiceMode::Smart => {
            return match service().plan_smart(text.trim()) {
                Ok(_) => true,
                Err(error) if error == x_agent::tools::NOT_A_COMMAND => false,
                Err(error) => {
                    service().emit("error", &error);
                    true
                }
            };
        }
        VoiceMode::Actions => (Mode::Actions, text.trim().to_string()),
        VoiceMode::Work => (Mode::Work, text.trim().to_string()),
        VoiceMode::Auto => match spoken_command(text) {
            Some(found) => found,
            None => return false,
        },
    };
    match service().plan(&command, mode) {
        Ok(_) => true,
        Err(error) if error.starts_with("Not understood") && mode == Mode::Actions => {
            service().emit(
                "info",
                "Not a command X knows, so it was typed as dictation",
            );
            false
        }
        Err(error) => {
            service().emit("error", &error);
            true
        }
    }
}

fn toast(title: &str, body: &str) {
    let brand = |text: &str| {
        if crate::app_paths::is_x_build() {
            text.replace("Hex", "X")
        } else {
            text.to_string()
        }
    };
    if let Ok(mut child) = Command::new("notify-send")
        .args(["-a", "X", "-t", "5000", "--"])
        .arg(brand(title))
        .arg(brand(body))
        .stdin(Stdio::null())
        .spawn()
    {
        thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

const APPROVAL_WAIT: Duration = Duration::from_secs(60);
/// Some notification daemons answer an action prompt themselves; a click that fast is not a person.
const HUMAN_FASTEST: Duration = Duration::from_millis(900);

/// Shows Allow and Deny in a desktop notification for this exact task. Ends, and
/// denies, when the wait runs out or the task is no longer the one awaiting approval.
fn ask_approval(svc: &Service, id: u64, summary: String) {
    let Ok(mut child) = Command::new("notify-send")
        .args([
            "-a",
            "X",
            "-u",
            "critical",
            "-A",
            "allow=Allow",
            "-A",
            "deny=Deny",
            "--wait",
            "--",
        ])
        .arg("X needs your approval")
        .arg(&summary)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
    else {
        svc.emit(
            "info",
            "No notification service; approve in the Agent pane or with `hex agent approve`",
        );
        return;
    };
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .args(["window", "show", "agent"])
            .stdin(Stdio::null())
            .spawn();
    }
    let shown = Instant::now();
    let deadline = shown + APPROVAL_WAIT;
    loop {
        let snapshot = svc.snapshot(0);
        let current = snapshot.phase == x_agent::phase::Phase::AwaitingApproval
            && snapshot.task.as_ref().is_some_and(|task| task.id == id);
        if !current || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            if current {
                svc.cancel();
                svc.emit("info", "Approval timed out; nothing ran");
            }
            return;
        }
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        thread::sleep(Duration::from_millis(150));
    }
    let mut choice = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut out, &mut choice);
    }
    let _ = child.wait();
    if shown.elapsed() < HUMAN_FASTEST {
        svc.emit(
            "info",
            "The notification service answered before anyone could read it, so it was ignored; use Approve in the Agent pane",
        );
    } else if choice.trim() == "allow" {
        if let Err(error) = svc.approve(id) {
            svc.emit("error", &error);
        }
    } else {
        svc.cancel();
        svc.emit("info", "Denied; nothing ran");
    }
}

/// Mirrors X's state into desktop notifications: an approval prompt for each plan that
/// waits, and a line for each result or error, so nothing needs the terminal.
fn watch(svc: &Service) {
    let mut seen = svc.snapshot(0).events.last().map_or(0, |event| event.n);
    let mut asked = 0;
    loop {
        thread::sleep(Duration::from_millis(200));
        if !crate::labs::x_agent() {
            continue;
        }
        let snapshot = svc.snapshot(seen);
        if let Some(last) = snapshot.events.last() {
            seen = last.n;
        }
        for event in &snapshot.events {
            if event.kind == "approval"
                && let Some(path) = approval_audit_file()
            {
                let line = serde_json::json!({"timestamp": event.t, "event": event.text});
                if let Ok(mut bytes) = serde_json::to_vec(&line) {
                    bytes.push(b'\n');
                    if let Err(error) = append_private(&path, &bytes) {
                        tracing::warn!(%error, "could not append X approval audit");
                    }
                }
            }
            match event.kind.as_str() {
                "error" => toast("X: problem", &event.text),
                "permission" if event.text.starts_with("Blocked ") => toast(
                    "X: OpenCode was blocked",
                    &format!("{}. Allow it once from the Agent pane.", event.text),
                ),
                "info" if event.text == "Task completed" => {
                    if let Some(task) = &snapshot.task {
                        toast("X: done", task.summary.as_deref().unwrap_or(&task.text));
                    }
                }
                _ => {}
            }
        }
        let Some(task) = snapshot.task.as_ref() else {
            continue;
        };
        if snapshot.phase == x_agent::phase::Phase::AwaitingApproval && task.id != asked {
            asked = task.id;
            let steps: Vec<_> = task.steps.iter().map(|step| step.title.as_str()).collect();
            let summary = format!("{}\n{}", task.text, steps.join("\n"));
            let svc = svc.clone();
            let id = task.id;
            thread::spawn(move || ask_approval(&svc, id, summary));
        }
    }
}

fn snapshot(since: u64) -> ControlReply {
    match serde_json::to_value(service().snapshot(since)) {
        Ok(snapshot) => ControlReply::Agent { snapshot },
        Err(error) => ControlReply::error("failed", format!("couldn't read X's state: {error}")),
    }
}

/// Runs `work` off the control thread: voice calls re-enter Hex's own control socket
/// (`hex dictation …`), which would deadlock a handler that waited on them.
fn background(work: impl FnOnce(&Service) + Send + 'static) {
    thread::spawn(move || work(service()));
}

pub fn handle(action: AgentAction) -> ControlReply {
    if !crate::labs::x_agent() {
        return ControlReply::error("disabled", DISABLED);
    }
    let svc = service();
    match action {
        AgentAction::Status { since } => snapshot(since),
        AgentAction::Toggle { mode } => match Mode::parse(&mode) {
            Err(error) => ControlReply::error("usage", error),
            Ok(mode) => {
                let listening = svc.snapshot(0).phase == x_agent::phase::Phase::Listening;
                background(move |svc| {
                    let result = if listening {
                        svc.voice_stop()
                    } else {
                        svc.voice_start(mode)
                    };
                    if let Err(error) = result {
                        svc.emit("error", &error);
                    }
                });
                ControlReply::done(if listening {
                    "Transcribing"
                } else {
                    "Listening"
                })
            }
        },
        AgentAction::Say { text, mode } => match Mode::parse(&mode) {
            Err(error) => ControlReply::error("usage", error),
            Ok(mode) if mode == Mode::Actions && svc.needs_model(&text) => {
                background(move |svc| {
                    if let Err(error) = svc.plan(&text, mode) {
                        svc.emit("error", &error);
                    }
                });
                ControlReply::done("Planning with the model; watch `x agent status`")
            }
            Ok(mode) => match svc.plan(&text, mode) {
                Ok(_) => snapshot(0),
                Err(error) => ControlReply::error("refused", error),
            },
        },
        AgentAction::Approve { id } => match svc.approve(id) {
            Ok(()) => ControlReply::done("Approved; running"),
            Err(error) => ControlReply::error("refused", error),
        },
        AgentAction::Stop => {
            background(Service::cancel);
            ControlReply::done("Stop requested")
        }
        AgentAction::Trust { on } => set_trust(on),
        AgentAction::Planner { model } => set_planner(&model),
        AgentAction::Desktop => {
            let r = x_agent::wm::report();
            ControlReply::done(serde_json::to_string(&r).unwrap_or_default())
        }
        AgentAction::Context => {
            let engine = svc.context_engine();
            let report = serde_json::json!({
                "capabilities": engine.capabilities(),
                "context": engine.capture(true).summary(),
            });
            ControlReply::done(report.to_string())
        }
        AgentAction::Wake { command, value } => wake_words_command(&command, &value),
        AgentAction::Computer { command, value } => computer_access(&command, &value),
        AgentAction::Allow { index } => match svc.allow_once(index) {
            Ok(_) => snapshot(0),
            Err(error) => ControlReply::error("refused", error),
        },
        AgentAction::Voice { mode } => match VoiceMode::parse(&mode) {
            Some(mode) => set_voice_mode(mode),
            None => ControlReply::error("usage", "mode is dictate, actions, work, auto or smart"),
        },
        AgentAction::Workspace { path } => match svc.set_workspace(&path) {
            Ok(shown) => ControlReply::done(format!("Workspace is {shown}")),
            Err(error) => ControlReply::error("refused", error),
        },
    }
}

/// Hex is quitting: stop any run and drop any recording first. Bounded, so a stuck child
/// can never keep Hex from exiting (the run's process group has already been signalled).
pub fn shutdown() {
    if !crate::labs::x_agent() {
        return;
    }
    let (done, wait) = std::sync::mpsc::channel();
    thread::spawn(move || {
        service().shutdown();
        let _ = done.send(());
    });
    let _ = wait.recv_timeout(std::time::Duration::from_secs(3));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mode_needs_the_spoken_prefix() {
        assert_eq!(
            spoken_command("Hey X, open Firefox."),
            Some((Mode::Actions, "open Firefox".into()))
        );
        assert_eq!(
            spoken_command("Hey Friday, open Firefox."),
            Some((Mode::Actions, "open Firefox".into()))
        );
        assert_eq!(
            spoken_command("Friday, go to workspace 3."),
            Some((Mode::Actions, "go to workspace 3".into()))
        );
        assert_eq!(
            spoken_command("Hey Friday, use Chrome to find the latest pull request."),
            Some((
                Mode::Actions,
                "use Chrome to find the latest pull request".into()
            ))
        );
        assert_eq!(
            spoken_command("friday work fix the failing tests"),
            Some((Mode::Work, "fix the failing tests".into()))
        );
        assert_eq!(
            spoken_command("x work fix the failing tests"),
            Some((Mode::Work, "fix the failing tests".into()))
        );
        for plain in [
            "open firefox",
            "xylophone open it",
            "next steps",
            "hey there x",
            "fridays are great",
            "friday",
            "x",
            "hey x,",
        ] {
            assert_eq!(spoken_command(plain), None, "{plain}");
        }
    }
}

//! X command options and their existing service/control route.

use super::*;

#[derive(Subcommand, Debug)]
pub enum AgentKeyCommand {
    /// Read one OpenRouter key from stdin; use a pipe to avoid argv and shell history.
    Set,
    /// Show whether the owner-only key file exists, without revealing its contents.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum AgentCommand {
    /// Linux task popups contain task numbers and outcomes only; details stay in X.
    Notifications {
        #[arg(value_parser = ["on", "off", "status"], default_value = "status")]
        mode: String,
        #[command(flatten)]
        output: Output,
    },
    /// Clear finished in-memory task history. Active work and recovery records stay available.
    ClearActivity {
        #[command(flatten)]
        output: Output,
    },
    /// Local, opt-in review of heard Actions speech.
    Heard {
        #[arg(value_parser = ["on", "off", "list", "promote", "clear"])]
        command: String,
        index: Option<usize>,
    },
    /// Store the Decider key securely from standard input, or show whether
    /// one is configured. The actual credential is never printed or passed in argv.
    Key {
        #[command(subcommand)]
        command: AgentKeyCommand,
    },
    /// Show X's phase, the current task, and recent events.
    Status {
        /// Only events after this number.
        #[arg(long, default_value_t = 0)]
        since: u64,
        #[command(flatten)]
        output: Output,
    },
    /// Start listening, or stop listening and plan what was said. Default mode
    /// asks for approval; Autonomous mode can start safe Actions immediately.
    /// The transcript never pastes into the focused app.
    Toggle {
        /// `actions` (open apps and sites) or `work` (hand a goal to OpenCode).
        #[arg(long, default_value = "actions")]
        mode: String,
        #[command(flatten)]
        output: Output,
    },
    /// Plan typed text exactly as spoken text would be.
    Say {
        #[arg(long, default_value = "actions")]
        mode: String,
        #[arg(required = true)]
        text: Vec<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Run the planned task with this id.
    Approve {
        id: u64,
        /// Keep coding or test work active while the foreground handles other commands.
        #[arg(long)]
        background: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Answer the current computer question, then review with approve-answer.
    Answer {
        id: u64,
        #[arg(long)]
        question: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Resume the exact answered computer question after review.
    ApproveAnswer {
        id: u64,
        #[arg(long)]
        question: String,
        #[command(flatten)]
        output: Output,
    },
    /// Choose one of the numbered windows X listed in the last 30 seconds.
    Choose {
        index: u8,
        #[command(flatten)]
        output: Output,
    },
    /// Undo X's most recent verified reversible window change, when still fresh.
    Undo {
        #[command(flatten)]
        output: Output,
    },
    /// Stop all owned tasks, or only the selected foreground/background run.
    Stop {
        #[arg(long, conflicts_with = "run")]
        foreground: bool,
        #[arg(long)]
        run: Option<u64>,
        #[command(flatten)]
        output: Output,
    },
    /// Stop one coding job in an exact task (job numbers begin at one).
    StopJob {
        id: u64,
        #[arg(value_parser = clap::value_parser!(u8).range(1..))]
        job: u8,
        #[command(flatten)]
        output: Output,
    },
    /// `on` runs allowlisted Actions (open an app, open or search a public site) as soon as
    /// they are planned; `off` asks first. Work always asks.
    Trust {
        #[arg(value_parser = ["on", "off"])]
        state: String,
        #[command(flatten)]
        output: Output,
    },
    /// How X runs desktop commands: `default` asks each time, `autonomous`
    /// auto-starts safe Actions and explicitly granted screen sessions without
    /// a separate prompt. Work, destructive actions and missing grants still ask.
    /// `status` reports the active policy. This reuses the legacy trust setting.
    Autonomy {
        #[arg(value_parser = ["default", "autonomous", "status"])]
        mode: String,
        #[command(flatten)]
        output: Output,
    },
    /// What your dictation shortcut does with speech: `dictate` types it (Hex as usual),
    /// `actions` or `work` send it to X, `auto` sends only speech that starts with "Friday" or
    /// "hey Friday" (or "x") and types the rest, and `smart` lets the planner model decide each time whether
    /// you gave a command or are dictating (needs a planner; unsure counts as dictation).
    Voice {
        #[arg(value_parser = ["dictate", "actions", "work", "auto", "smart"])]
        mode: String,
        #[command(flatten)]
        output: Output,
    },
    /// Linux hands-free listening. `on` requires a wake word. `smart` previews local
    /// desktop commands without one. `off` releases the microphone.
    Listen {
        #[arg(value_parser = ["on", "smart", "off", "status"])]
        mode: String,
        #[command(flatten)]
        output: Output,
    },
    /// Keep the desktop awake for a bounded duration, stop it or check remaining time.
    /// Uses the OS's existing native recording-inhibitor backend without recording.
    Awake {
        #[arg(value_parser = ["on", "off", "status", "until"])]
        command: String,
        #[arg(long, default_value_t = 60)]
        minutes: u32,
        /// With 'until': 24-hour target time HH:MM in the specified UTC offset.
        #[arg(long)]
        clock: Option<String>,
        /// With 'until': fixed UTC offset such as +05:30 (no DST inference).
        #[arg(long)]
        utc_offset: Option<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Rerun the last Work goal with command number `index` (from `status`, 0 first) that
    /// OpenCode was refused allowed for that one run. The rerun still asks for approval.
    Allow {
        index: usize,
        #[command(flatten)]
        output: Output,
    },
    /// Which window-control backend this desktop has and what it can do.
    Desktop {
        #[command(flatten)]
        output: Output,
    },
    /// Read-only first-run diagnostic report: compositor, helpers, session variables,
    /// installed applications, keyboard shortcuts, and remote key permissions.
    /// This runs locally even when X's background service is not running.
    Doctor {
        #[arg(long)]
        json: bool,
        /// Probe the already-running X service instead of this shell.
        #[arg(long)]
        running: bool,
    },
    /// Preview a Claude Code task in the selected workspace. Use --resume for the last X-owned session.
    Claude {
        #[arg(long)]
        resume: bool,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Plan a review of a manually supplied web page's URL and title.
    /// Query and fragment parameters are removed before it reaches Work.
    SendBrowserPage {
        url: String,
        #[arg(long, default_value = "")]
        title: String,
        #[command(flatten)]
        output: Output,
    },
    /// Share owner-selected terminal stdout/stderr as untrusted Work context.
    /// Use --stdin for up to 1200 bytes piped by the user; X runs no commands.
    SendTerminalOutput {
        #[arg(long)]
        stdin: bool,
        #[arg(num_args = 0..)]
        text: Vec<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Preview the selected text in the focused, granted browser for OpenCode.
    SendBrowserSelection {
        #[command(flatten)]
        output: Output,
    },
    /// Plan a bounded project file-line excerpt for the selected OpenCode session.
    SendProjectFile {
        path: String,
        #[arg(long, default_value_t = 1)]
        line: u32,
        #[arg(long, default_value_t = 10)]
        count: u32,
        #[command(flatten)]
        output: Output,
    },
    /// List or select an existing OpenCode session for the chosen workspace.
    Sessions {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Register, list, or forget explicitly chosen project directories.
    Projects {
        #[command(subcommand)]
        command: ProjectCli,
    },
    /// Saved desktop workflows, run by trigger phrase or by id. `list` shows them with their
    /// last result; `run <id>` plans a run (approve it as usual); `import <file>` adds one from a
    /// JSON file after the same validation as the editor; `undo` puts back the last run's changes.
    Workflow {
        #[arg(value_parser = ["list", "show", "preview", "permissions", "uncertain", "claims", "worktrees", "run", "run-graph", "test-project", "last", "last-graph", "delete", "duplicate", "import", "import-graph", "undo", "dir"])]
        command: String,
        /// A workflow/project id, an optional worktrees run UUID, or an import path.
        #[arg(default_value = "")]
        value: String,
        /// Test preset for 'test-project' only.
        #[arg(default_value = "")]
        suite: String,
        #[command(flatten)]
        output: Output,
    },
    /// Plan a clean fast-forward of a task branch into its primary checkout.
    /// Requires project/run/task/start commit and separate owner approval.
    IntegrateFastForward {
        project: String,
        run_id: String,
        task_id: String,
        start_commit: String,
        #[command(flatten)]
        output: Output,
    },
    /// What X can see right now: the focused window and whether text is selected. The
    /// selected text itself is never printed.
    Context {
        #[command(flatten)]
        output: Output,
    },
    /// The word that starts a spoken command, in `auto` voice mode. Pick any single word:
    /// `set jarvis` replaces them, `add computer` adds one, `remove` drops one, `list` shows them.
    Wake {
        #[arg(value_parser = ["list", "add", "remove", "set"])]
        command: String,
        /// The word, for add, remove and set.
        #[arg(default_value = "")]
        value: String,
        #[command(flatten)]
        output: Output,
    },
    /// What X may read and operate on screen. `grant chrome` lets X read that app's windows
    /// and click and type in them; `revoke` takes it back; `list` shows both; `remote on`
    /// allows on-screen text to be sent to a model that is not on this machine (default off).
    Computer {
        #[arg(value_parser = ["grant", "revoke", "list", "remote", "vision"])]
        command: String,
        /// An app name for grant and revoke, `on` or `off` for remote.
        #[arg(default_value = "")]
        value: String,
        #[command(flatten)]
        output: Output,
    },
    /// Choice-only free Perplexity Decider providers: decider-flash, decider, or off.
    /// Gemini, OpenAI-compatible, Ollama and legacy Jev settings are unsupported.
    Planner {
        model: String,
        #[command(flatten)]
        output: Output,
    },
    /// Save a local screenshot using the platform's supported capture modes.
    /// Captures are never uploaded. Existing output files are never overwritten.
    Screenshot {
        #[arg(default_value = "screen", value_parser = ["screen", "window", "region"])]
        mode: String,
        /// Optional platform-native PNG or BMP path (default: a new file in Pictures).
        #[arg(long)]
        output: Option<std::path::PathBuf>,
    },
    /// The project folder Work runs in (never `/` or your home directory).
    Workspace {
        path: String,
        #[command(flatten)]
        output: Output,
    },
}

pub(super) fn run(command: AgentCommand) -> u8 {
    if let AgentCommand::Heard { command, index } = &command {
        return match crate::agent_host::heard::command(command, *index) {
            Ok(message) => {
                println!("{message}");
                0
            }
            Err(error) => {
                eprintln!("{error}");
                EXIT_REFUSED
            }
        };
    }
    if let AgentCommand::Key { command } = &command {
        return match command {
            AgentKeyCommand::Set => match set_agent_key() {
                Ok(()) => {
                    println!("OpenRouter key stored in X's private data folder");
                    0
                }
                Err(error) => {
                    eprintln!("{error}");
                    EXIT_REFUSED
                }
            },
            AgentKeyCommand::Status => {
                let present = crate::app_paths::support_dir()
                    .ok()
                    .map(|dir| dir.join("openrouter-key"))
                    .is_some_and(|path| path.is_file());
                println!(
                    "OpenRouter key: {}",
                    if present {
                        "configured"
                    } else {
                        "not configured"
                    }
                );
                0
            }
        };
    }
    if let AgentCommand::Doctor { json, running } = &command {
        let reply = if *running {
            call(ControlRequest::new(ControlCommand::Agent {
                action: AgentAction::Doctor,
            }))
            .map_or_else(|reply| reply, |response| response.reply)
        } else {
            crate::agent_host::doctor_report(x_agent::wm::ProbeSource::Cli)
        };
        return human::print_doctor(reply, *json);
    }
    if let AgentCommand::Screenshot { mode, output } = &command {
        let mode = match x_agent::screenshots::Mode::parse(mode) {
            Ok(mode) => mode,
            Err(error) => {
                eprintln!("{error}");
                return EXIT_REFUSED;
            }
        };
        return match x_agent::screenshots::save(mode, output.as_deref()) {
            Ok(path) => {
                println!("{}", path.display());
                0
            }
            Err(error) => {
                eprintln!("{error}");
                EXIT_REFUSED
            }
        };
    }
    let (action, output) = match command {
        AgentCommand::Notifications { mode, output } => {
            (AgentAction::Notifications { mode }, output)
        }
        AgentCommand::ClearActivity { output } => (AgentAction::ClearActivity, output),
        AgentCommand::Status { since, output } => (AgentAction::Status { since }, output),
        AgentCommand::Toggle { mode, output } => (AgentAction::Toggle { mode }, output),
        AgentCommand::Say { mode, text, output } => (
            AgentAction::Say {
                text: text.join(" "),
                mode,
            },
            output,
        ),
        AgentCommand::Approve {
            id,
            background,
            output,
        } => (
            if background {
                AgentAction::ApproveBackground { id }
            } else {
                AgentAction::Approve { id }
            },
            output,
        ),
        AgentCommand::Answer {
            id,
            question,
            text,
            output,
        } => (
            AgentAction::AnswerComputer {
                id,
                question,
                text: text.join(" "),
            },
            output,
        ),
        AgentCommand::ApproveAnswer {
            id,
            question,
            output,
        } => (AgentAction::ApproveComputerAnswer { id, question }, output),
        AgentCommand::Choose { index, output } => (AgentAction::Choose { index }, output),
        AgentCommand::Undo { output } => (AgentAction::Undo, output),
        AgentCommand::Stop {
            foreground,
            run,
            output,
        } => (
            if foreground {
                AgentAction::StopForeground
            } else if let Some(id) = run {
                AgentAction::StopRun { id }
            } else {
                AgentAction::Stop
            },
            output,
        ),
        AgentCommand::StopJob { id, job, output } => (
            AgentAction::StopJob {
                id,
                index: usize::from(job - 1),
            },
            output,
        ),
        AgentCommand::Claude {
            resume,
            text,
            output,
        } => (
            AgentAction::Claude {
                text: text.join(" "),
                resume,
            },
            output,
        ),
        AgentCommand::Sessions { command } => match command {
            SessionCommand::List { output } => (AgentAction::Sessions { resume: None }, output),
            SessionCommand::Resume { id, output } => {
                (AgentAction::Sessions { resume: Some(id) }, output)
            }
            SessionCommand::Send { text, output } => (
                AgentAction::SendToSession {
                    text: text.join(" "),
                },
                output,
            ),
        },
        AgentCommand::SendBrowserSelection { output } => {
            (AgentAction::SendBrowserSelection, output)
        }
        AgentCommand::Projects { command } => {
            use crate::control::ProjectCommand;
            let (command, output) = match command {
                ProjectCli::List { output } => (ProjectCommand::List, output),
                ProjectCli::Register { name, path, output } => {
                    (ProjectCommand::Register { name, path }, output)
                }
                ProjectCli::Use { query, output } => (ProjectCommand::Use { query }, output),
                ProjectCli::AliasAdd { id, alias, output } => {
                    (ProjectCommand::AliasAdd { id, alias }, output)
                }
                ProjectCli::AliasRemove { id, alias, output } => {
                    (ProjectCommand::AliasRemove { id, alias }, output)
                }
                ProjectCli::Forget { id, output } => (ProjectCommand::Forget { id }, output),
            };
            (AgentAction::Projects { command }, output)
        }
        AgentCommand::Desktop { output } => (AgentAction::Desktop, output),
        AgentCommand::Doctor { .. } => unreachable!("doctor handled locally"),
        AgentCommand::Heard { .. } => unreachable!("heard handled locally"),
        AgentCommand::Key { .. } => unreachable!("key handled locally"),
        AgentCommand::Context { output } => (AgentAction::Context, output),
        AgentCommand::IntegrateFastForward {
            project,
            run_id,
            task_id,
            start_commit,
            output,
        } => (
            AgentAction::IntegrateFastForward {
                project,
                run_id,
                task_id,
                start_commit,
            },
            output,
        ),
        AgentCommand::SendBrowserPage { url, title, output } => {
            (AgentAction::SendBrowserPage { url, title }, output)
        }
        AgentCommand::SendTerminalOutput {
            stdin,
            text,
            output,
        } => {
            use std::io::Read;
            let excerpt = if stdin {
                if !text.is_empty() {
                    return fail("Use either --stdin or a literal excerpt");
                }
                let mut bytes = Vec::new();
                if std::io::stdin().take(1201).read_to_end(&mut bytes).is_err() {
                    return fail("Cannot read the requested terminal input");
                }
                match String::from_utf8(bytes) {
                    Ok(content) => content,
                    Err(_) => return fail("Terminal output must be UTF-8 text"),
                }
            } else {
                text.join(" ")
            };
            if excerpt.is_empty() || excerpt.len() > 1200 {
                return fail("Choose 1-1200 bytes of terminal output");
            }
            (AgentAction::SendTerminalOutput { text: excerpt }, output)
        }
        AgentCommand::SendProjectFile {
            path,
            line,
            count,
            output,
        } => (
            AgentAction::SendProjectFile {
                path,
                start_line: line,
                line_count: count,
            },
            output,
        ),
        AgentCommand::Workflow {
            command,
            mut value,
            suite,
            output,
        } => {
            if command == "test-project" {
                if value.is_empty() || suite.is_empty() {
                    return fail("Usage: agent workflow test-project <project-id> <suite>");
                }
                value = format!("{value} {suite}");
            } else if !suite.is_empty() {
                return fail("Unexpected second argument for this workflow command");
            }
            if matches!(command.as_str(), "import" | "import-graph") && !value.is_empty() {
                let Ok(path) = std::path::absolute(&value) else {
                    return fail("Could not resolve the workflow file");
                };
                let Ok(path) = path.into_os_string().into_string() else {
                    return fail("The workflow file path must be valid UTF-8");
                };
                value = path;
            }
            (AgentAction::Workflow { command, value }, output)
        }
        AgentCommand::Wake {
            command,
            value,
            output,
        } => (AgentAction::Wake { command, value }, output),
        AgentCommand::Computer {
            command,
            value,
            output,
        } => (AgentAction::Computer { command, value }, output),
        AgentCommand::Planner { model, output } => (AgentAction::Planner { model }, output),
        AgentCommand::Screenshot { .. } => unreachable!("handled locally above"),
        AgentCommand::Allow { index, output } => (AgentAction::Allow { index }, output),
        AgentCommand::Voice { mode, output } => (AgentAction::Voice { mode }, output),
        AgentCommand::Listen { mode, output } => (AgentAction::Listen { mode }, output),
        AgentCommand::Awake {
            command,
            minutes,
            clock,
            utc_offset,
            output,
        } => (
            AgentAction::Awake {
                command,
                minutes,
                clock,
                utc_offset,
            },
            output,
        ),
        AgentCommand::Trust { state, output } => (AgentAction::Trust { on: state == "on" }, output),
        AgentCommand::Autonomy { mode, output } => (AgentAction::Autonomy { mode }, output),
        AgentCommand::Workspace { path, output } => {
            let absolute = std::path::absolute(&path).map_or(path, |p| p.display().to_string());
            (AgentAction::Workspace { path: absolute }, output)
        }
    };
    send(ControlCommand::Agent { action }, output)
}

/// Reject control characters, shell/curl metacharacters and oversized keys
/// before writing anything to disk. The bytes are never included in errors.
pub(super) fn validate_agent_key(bytes: &[u8]) -> Result<&str, String> {
    if bytes.len() > 4096 {
        return Err("OpenRouter key exceeds 4096 bytes".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "OpenRouter key must be UTF-8")?;
    let key = text.trim_end_matches(['\r', '\n']);
    if key.is_empty()
        || key.len() > 4096
        || !key
            .chars()
            .all(|ch| ch.is_ascii_graphic() && !matches!(ch, '"' | '\'' | '\\'))
    {
        return Err("OpenRouter key has invalid characters or is empty".into());
    }
    Ok(key)
}

pub(super) fn save_agent_key(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    let key = validate_agent_key(contents)?;
    let pending = path.with_extension(format!("pending-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&pending)?;
        file.write_all(key.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&pending, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&pending);
    }
    result.map_err(|_| "Could not securely save the OpenRouter key".into())
}

fn set_agent_key() -> Result<(), String> {
    let dir = crate::app_paths::support_dir()
        .map_err(|_| "X's application data folder is unavailable")?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read key from standard input")?;
    save_agent_key(&dir.join("openrouter-key"), &bytes)
}

# X agent (Labs)

X turns Hex's local speech-to-text into **Actions** (open apps and sites) and **Work**
(hand a goal to OpenCode). Ordinary dictation is untouched: X has its own switch, its own
entry point, and never pastes what you say.

Off by default. Turn it on in Settings › Labs › X agent, or `hex labs on x-agent`.

## Modes

| Mode | What it does | Runs |
| --- | --- | --- |
| Dictate | Hex as before, on Hex's own shortcut | unchanged |
| Actions | "open Firefox and search YouTube for Mars rover" | a fixed set of typed launches; uncommon requests go to the selected planner |
| Work | "find the bug in this project and fix it" | `opencode run` in a folder you chose |
| Computer | "find the latest pull request on GitHub" | a granted browser, editor, or file manager, using accessibility controls and on-screen evidence |

### Execution policy: Default or Autonomous

X's *execution policy* is separate from the dictation shortcut's voice modes
(Dictate, Actions, Work, Auto, Smart).

| Execution mode | Behavior |
| --- | --- |
| **Default** | Every new desktop task pauses for approval before execution. |
| **Autonomous** | Low-risk local Actions start without per-task approval. A Jev computer-use task can also run immediately **only** when that app already has a specific screen grant and remote-screen permission has been explicitly enabled if needed. |

Choose in the **Agent pane → Execution** controls or by CLI:

```sh
hex agent autonomy status
hex agent autonomy autonomous  # opt in, persisted across X restarts
hex agent autonomy default     # return to manual approval
hex agent stop                 # stop the active task at any time
```

In Autonomous mode, each approved computer task can already run multiple
observe → decide → act → verify cycles without repeatedly asking. This does
**not** enable screen reading or remote transmission on its own. Grant apps
separately (`hex agent computer grant firefox`) and opt into remote screen
data separately (`hex agent computer remote on`). Computer tasks remain
bounded to the permitted app, 15 steps, three minutes and X's existing Stop
control. The agent still refuses payment, posting, deleting, accepting
permissions, password entry and other consequential UI actions. Work/OpenCode
jobs and risky or untrusted operations require human review.

The prior `hex agent trust on|off` command is maintained as an alias for
Autonomous/Default, using the same owner-controlled persisted setting; it
doesn't create a second conflicting mode. The default for a fresh install
is **Default**. A previously enabled Trusted Actions setting is interpreted
as Autonomous until switched off.

A task runs through the same phases, but Autonomous may skip
`awaiting_approval` on permitted tasks:

```
idle → listening → transcribing → planning → [approval if required] → running
     → completed | failed | cancelled
```

## Using it

```
hex agent workspace ~/code/project     # Work only: the folder OpenCode may change
hex agent toggle --mode work           # start listening; run again to stop and plan
hex agent say --mode actions open firefox   # type instead of speaking
hex agent status                       # phase, plan with its steps, recent events
hex agent approve 3                    # run task 3
hex agent stop                         # discard the plan, drop the recording, or stop the run
hex agent autonomy autonomous          # optional: safe actions run without repeated approval
```

The Agent pane in the window (sidebar, while the Labs switch is on) does the same with
buttons and shows the plan, steps, changed files, and event stream.

**Voice routing.** The Agent pane controls the dictation shortcut's mode: Dictate, Actions,
Work, Auto, or Smart. Auto only routes speech beginning with Friday or X. Smart asks the
planner whether speech is a command. A planner error is reported and consumed; it is not
passed to the focused app as dictation. Escape cancels an active Agent task while the Agent
pane is selected.

**The Agent shortcut.** Bind `hex agent toggle` (and `hex agent stop`) to a key in your
desktop or compositor. Dictation's own shortcut keeps working, and the two never record at
once: X refuses to start while any task or recording of its own is in flight.

### Jev-only command decisions (opt-in)

X can use OpenCode Console's **Jev 1.13 Free** decision model for Actions
without enabling a general-purpose chat LLM. It is NOT a text generator:
it selects exactly one existing, validated candidate action, or says the
utterance was dictation/unsupported. Existing deterministic rules still
run locally first, without an API call. Work-mode OpenCode tasks still
require whatever coding model OpenCode uses.

```sh
hex labs on x-agent
hex agent planner jev
hex agent voice auto
hex agent say --mode actions "Could you bring Firefox to the front?"
```

Provide an OpenCode Console API key through `JEV_API_KEY` (or
`OPENCODE_API_KEY`) in the **running app/service environment**, or put
the key in the owner-only `jev-key` file inside Hex/X's application data
folder (`app_paths::support_dir`). An export in a terminal may not reach
an already running systemd service. Do not commit the key into the repo.
`hex agent planner off` disables remote routing again.

The first decision (simple action routing) sends only the spoken transcript.
When you explicitly grant **Computer** access for an app and opt into remote
screen analysis, the multi-step agent additionally sends the current
accessibility tree's labels/roles, redacted visible text, window title and
recent action outcomes to the fixed OpenCode HTTPS endpoint. No screenshots,
clipboard contents, passwords, selected text or files are sent by this planner.
Long tokens, email addresses and long numeric strings are redacted before
the remote request. The free model is a promotional offering and may change.

### Jev-only observe → decide → act → verify

The Jev planner now has its own typed computer decisions, so it **does not**
require a conversational LLM or a generated JSON action script. For a
natural multi-step browser request, X opens/locates the permitted browser
and repeatedly:

1. Reads the current accessible controls and page text.
2. Asks Jev which offered operation and *current element* to use.
3. Re-checks the decision's probability, target and permission, then acts.
4. Re-reads the actual interface, checks what changed and repeats.
5. Stops on verified completion, user Stop, an unsafe action, repeated
   failure, 15 steps or a three-minute deadline.

When typing, Jev selects word boundaries in **your own goal**. X copies
the exact substring, never text generated by Jev. Jev can choose a control
by its current snapshot ID, a public website explicitly named in the goal,
safe browser keys, scrolling or a visible completion evidence label.
The executor refuses destructive, security-sensitive, sending, buying,
deleting, publishing and approval controls, and never runs shell commands.
`done` requires visible evidence; the claim is rejected if it was
already present before the task's actions.

Enable remote screen context **explicitly**, on top of X's normal Agent
Labs switch and planner selection:

```sh
hex labs on x-agent
hex agent planner jev
hex agent computer grant firefox
hex agent computer remote on
hex agent say --mode actions "Open GitHub, find my latest PR and check its status"
```

Use `hex agent computer grant chrome`, `brave`, `chromium`, `vscode`
or `files` for another supported app. Unlike browsers, VS Code and file
managers cannot use browser-navigation shortcuts and their UI automation
is limited to allowed controls and literal text. Terminal access is not
offered to the computer-use planner. `hex agent computer remote off`
retracts remote accessibility permission; revoking a particular app's
grant also stops future planning for that app.

**Limits:** X currently operates one explicitly granted app per task; it
cannot coordinate across several apps in the same computer loop, synthesize
original prose, summarize a page, reason through coding tasks, or complete
payments/posts. A question requiring a generative answer or writing new
content still needs a separate model. Jev errors and low-confidence
choices fail closed rather than guessing or falling back to paid inference.
Deterministic commands still run without contacting the provider.
Work-mode OpenCode still needs its own configured coding model.

Source: `crates/x-agent/src/jev.rs`, `crates/x-agent/src/jev_computer.rs`,
`crates/x-agent/src/computer.rs` and `src/agent_host.rs`.
Synthetic unit tests validate offered choices, selection safety and
computer-loop behavior. A real OpenCode Console key, accessible desktop
and human review are still required for live end-to-end proof.

## X Quick (local action search)

The Agent pane's **Quick** tab searches the existing approved app registry,
workspace switches (1–10), and runnable saved workflows. Search matches are
ranked deterministically on the current machine. It does not run a language
model and never turns a result label into a shell command. When no exact
local action matches, Quick also offers an **explicit** web-search row (never
opens a URL without a click). Prefix queries with `gh `, `yt `, `g `, `w `,
`r `, or `so ` for GitHub, YouTube, Google, Wikipedia, Reddit, or Stack Overflow.
This uses the existing typed Search action and its normal approval gate.

- Opening or focusing an app and switching workspace goes through the existing
  Actions planner, showing the same plan and approval controls as spoken input.
- Saved workflows go through their existing validator, preflight and approval
  path. Invalid or conflicting workflow entries are omitted from Quick.
- The workflow list is reused from the manager's short-lived cache; no workflow
  files are reread for every search keystroke.
- The existing window-control API now exposes `hex window show quick`. Bind
  that command to a desktop shortcut to jump straight into the Quick tab
  from another app without adding a new global keyboard interceptor.
- Prefix `read ` or `view ` to search the same scoped folders for
  ordinary files that can be previewed in X Quick. Clicking **Preview** reads
  up to 8 KiB of one allowlisted text file, displays at most 2,000 characters
  in memory, and offers a Close preview control. It does not send text to
  an agent, remote model, external editor, or clipboard. Binary files,
  symlinks, scripts, unsupported extensions, and large files are refused.
- Prefix a query with `file ` (or `f `) to search filenames only within
  common home folders. The scanner is bounded (6,000 entries, three nested
  directory levels, 70 ms) and skips hidden paths, symlinks and build caches.
  Explicitly opening a result revalidates its location and rejects executable
  files. No indexing runs in the background and file contents are never read.
- `convert 09:00 +05:30 to -07:00` converts a named clock time between
  two **explicit fixed UTC offsets**, showing previous/next-day rollover.
  Calculations are local and never infer daylight-saving rules or IANA
  timezone names (e.g. `PST` is refused rather than guessed).
- A query starting with `=` or `calc ` evaluates bounded arithmetic entirely locally (parentheses, +, -, *, /, %, exponentiation); invalid calculations are refused instead of guessed.
- Explicit date/time expressions such as `date today`, `date +10`,
  `date yesterday`, `time utc`, and `time +05:30` calculate dates
  and fixed-offset clock times locally. Results are UTC-based, not a guess
  about the user's timezone; named timezones, daylight-saving rules and
  natural-language date questions are not implemented.

- A HEX or RGB color query (e.g. `color #2563EB` or `rgb(37, 99, 235)`) produces copyable HEX, RGB, HSL, and OKLCH values without a model or screen capture. `Copy` uses the existing Wayland clipboard action, which is not cross-platform yet.
- X Quick now offers **in-app navigation** to Settings, Formatting (Output),
  History, Commands and Workflows. Commands and Workflows reuse the existing
  Agent pane; navigation never goes through Jev or the Actions executor.
- Inside X's own window, `Ctrl+K` (Linux/Windows) or `Command+K` (macOS)
  toggles Quick. Escape closes Quick when no other higher-priority action
  (such as stopping a running agent task) applies. For a system-wide shortcut,
  continue to bind `hex window show quick` in the desktop's settings.
- Quick remains a **native pane tab**, not a separately floating launcher,
  and does not replace Hex's dictation shortcut.
- The search UI does not run an action automatically on selection; the button
  prepares a plan that can be reviewed in the Commands pane.

Source: `crates/x-agent/src/quick.rs`, `src/rune_ui/agent/quick.rs`,
`src/rune_ui/agent.rs`. Unit tests cover ranking and typed-action routing;
real key focus, UI accessibility, layout and desktop latency remain unverified.

## Offline documentation search in X Quick

Type `help <topic>` or `docs <topic>` in the Quick search pane to search
embedded, version-matched X/Hex help sections. Search results quote up to 550
characters of the shipped documentation and show an explicit **Read** button.
Queries and excerpts never leave the device; they do not open a website or call
an AI model. Only X actions, CLI, vocabulary and history documentation are
included. If a section is missing, X reports no result rather than making up an
answer. The `help ` and `docs ` prefixes do not offer web-search fallbacks.

## Retained dictation search from X Quick

Type `hist <words>` or `history <words>` in X Quick to search past
successful dictations already saved by Hex. X uses the existing retained-history
store, retention rules and local BM25/vocabulary ranking. Up to six results
show short previews and the source application. Clicking Copy uses X's existing
explicit copy action; no transcript is sent to a model for ranking.

The command requires at least three query characters. This is not a new
history recorder, not a search of discarded audio, and not a background index.
Typing a local-history/clipboard/snippet query also suppresses the ordinary
web-search suggestion to avoid accidentally offering private text to a browser.

## Dictation statistics from X Quick

Type `stats`, `dictation stats`, `history stats`, `stats today`,
`stats 7d` or `stats 30d` to open a read-only summary of retained
dictations in X Quick. It reuses Hex's `hex history stats` implementation:
word count, speaking speed when audio timing exists, number of dictations,
Output changes, day streak and top applications. The values respect the
existing History retention policy and local day computation. There is no
new statistics database, telemetry, cloud upload, or language-model call.

The existing **Dictation stats** Labs toggle must be enabled; Quick reports
that requirement instead of bypassing it. This view is text-only, not yet a
visual charts dashboard. Stats commands never fall through to web search.

## Timed Keep Awake

`hex agent awake on --minutes 60` holds an OS idle inhibitor for up to one
hour, while `hex agent awake off` releases it and `hex agent awake status`
reports the remaining duration. Durations must be 1–480 minutes.

You can also use `hex agent awake until --clock 18:30 --utc-offset +05:30` to
stop at the next 18:30 in a specified **fixed UTC offset** (not an inferred
region/timezone). The target must be within eight hours. A minute-level timer
may release the inhibitor up to 59 seconds after the target. DST changes are
not inferred; use the correct current offset.

The timer uses Hex's platform-native idle prevention but does **not** set
its recording-in-progress state or keep the speech model warm. On Linux,
logind may refuse blocking system sleep and allow only idle inhibition. This
is an explicit session control, not a calendar-based schedule. Nothing runs
until the user asks for it.

## Clipboard text snapshots (explicit opt-in)

X Quick can save **one** text clipboard item when you click **Save current text**.
The item is stored in a bounded owner-only local JSON file with a 24-hour
retention window, up to 100 items (16 KiB per text item). Search for a saved
item using `clip <words>` or `clipboard <words>`; use the same explicit
Copy action as other Quick values. Click **Erase saved snapshots** to remove
the entire saved store. Saving the same text again moves it to the top.

- **Session capture (Linux Wayland, optional):** The Quick pane also has
  **Capture changes · 10 min** and **Stop capture** controls. Clicking the
  first explicitly enables a 10-minute one-second text polling session,
  with a hard 30-minute per-session maximum. The session is never restored
  on app restart and stops when X exits. The Stop button cancels further reads
  and waits for any in-flight save to finish. Outside an enabled session,
  the original manual snapshots remain the only way to save clipboard text.
- **Privacy:** The watcher skips non-text offers, MIME types advertising
  password-manager or sensitive content, and likely high-entropy tokens.
  This is only best-effort: not every password manager marks clipboard
  offers, and heuristics cannot recognize all secrets. **Do not copy
  passwords while the session is active**. Nothing is uploaded to a model
  or cloud service. Saved text follows the existing 24-hour local retention
  policy and can be erased with **Erase saved snapshots**.
- Watcher sessions are Linux Wayland only (require `wl-paste`). X11, macOS
  and Windows do not yet have session monitoring. Images, files and rich
  clipboard formats are not captured by this text-only implementation.
- Capture currently needs `wl-paste` on Wayland and text/plain clipboard
  data. Copying results uses X's existing `wl-copy` support.
- The JSON store is local to `hex/x-clipboard-snapshots.json` in the user
  configuration folder; it is not uploaded or given to the AI planner.
- Retention is enforced when searching and saving; old bytes on disk remain
  until an updated save or explicit erase. Treat clipboard snapshots as
  sensitive, even when capture is manual.
- Future work: privacy-aware background monitoring with source-app filters,
  permission toggles, image/file types, and session-specific exclusion rules.

## Explicit local snippets in Quick

The Agent pane's Quick tab includes a small editor for named snippets. Save a
trigger (ASCII letters, numbers, `-`, `_`; max 32) and up to 4096 characters of
text. Search `snip <trigger>`, `snippet <trigger>`, or `snippets` and press Copy
on an entry. Removing a trigger deletes its stored text.

- Stored under the user's config directory, `hex/x-snippets.json`, as a local
  JSON file. Updates use temporary files and refuse symlinked storage.
- The library is bounded to 100 items. Malformed data is reported; it is never
  automatically overwritten during a Save.
- Only deliberate Quick searches read snippets; none are sent to a model.
- Snippets may contain `{{utc_date}}` (YYYY-MM-DD), `{{utc_time}}`
  (HH:MM), or `{{clipboard_snapshot}}` (the newest explicitly saved
  clipboard text). Values are resolved only when the user copies or inserts
  the snippet, not when they browse or save it. The latter field does not
  read the live clipboard and refuses if no stored snapshot exists.
- Rendered snippets are capped to 8000 characters. Fields inside substituted
  clipboard text are never evaluated again, and unknown placeholders remain
  literal. These are UTC fields, not implicit local time-zone guesses.
- Copy uses X's existing Wayland clipboard mechanism. On an unsupported
  platform the copy action reports an error rather than claiming success.
- A deliberate spoken instruction such as `insert snippet signature` or
  `expand snippet signature` looks up the saved trigger before any model-based
  rewriting and inserts the exact saved text. Ordinary dictation is never
  matched against snippet names and isn't changed by this feature.
- The spoken lookup is case-insensitive and requires the full trigger; missing
  snippets are not guessed. This is not yet a global keyboard expansion hook.
- **Not a keyboard text expander yet.** The editor can enter short strings,
  while the JSON store supports line breaks and tabs. System-wide expansion
  requires a separate opt-in keyboard-hook implementation and native tests.

Source: `crates/x-agent/src/quick/snippets.rs` and
`src/rune_ui/agent/quick.rs`.

## Process inspection (Linux)

Type `processes`, `process <name>`, or `ps <name>` in the Quick tab to see
up to six currently running processes owned by the signed-in user, ordered by
resident memory. It reads Linux `/proc` on demand with a strict time/entry
budget; it never runs a background process scanner.

- Click **End…** then **Confirm SIGTERM** to request termination. The first
  click does nothing to the process. The second validates the process's name,
  owner and kernel start timestamp again and uses a Linux pidfd to pin its
  identity against PID reuse before sending SIGTERM.
- Root-owned sessions, PID 1, and X's own process cannot be terminated.
- The UI reports signal delivery, **not verified process exit**. It never
  escalates to SIGKILL or runs shell commands.
- macOS and Windows do not yet have a native process viewer in this branch.
- CPU percentages and restarting arbitrary processes are not implemented;
  reconstructing launch arguments would be unsafe without an explicit app
  registry.

Source: `crates/x-agent/src/quick/processes.rs` and native Quick UI.
Native Linux process-control tests should run only on an isolated test session.

## What X will and won't do

* **Actions** are a closed registry: launch an approved app, open a public
  `https://` URL, or search one of five sites. A planned step is data, never a command line:
  no shell, no `sh -c`, search text is percent-encoded into one URL argument.
* **Launched is not verified.** X reports "launched"; it does not claim a page loaded.
* **Work never bypasses OpenCode's permissions.** X does not pass `--auto`. The run uses a
  private OpenCode server (`--standalone`) when the installed OpenCode has it. A run only
  counts as completed with exit code 0 **and** no error event; an error event ends the run.
* **Stop really stops**: SIGTERM to the run's process group, SIGKILL after 5 s. X then
  lists files whose git state changed (when the folder is a git repo). Nothing is rolled
  back, and X says so.
* **Local by default.** Speech uses Hex's local model. Screen text is sent to a local
  planner unless you explicitly allow remote screen access. X sends nothing else itself;
  OpenCode talks to whichever provider you configured.
* **Computer use** requires a per-app grant and runs a bounded observe, decide,
  act, verify loop. Browsers, VS Code and file managers are supported within
  their allowed accessibility controls; browser-only keyboard shortcuts never run in
  editors or file managers. Links and a small set of navigation buttons may be clicked;
  other buttons and consequential controls are refused. Remote models additionally
  require explicit `agent computer remote on`; on-screen context is redacted before
  transmission. Approval decisions are appended to a private local audit file.

## Not done yet

* A native listener shortcut inside Hex (today: bind `hex agent toggle` in your desktop).
* Real Firefox and Chrome profile workflows have not been verified on this build.
* HUD phase indicators for Agent work are not implemented; phase state is visible in the
  Agent pane.
* Permission prompts raised by OpenCode are shown as a stall warning after 120 s of silence,
  not yet as an in-pane approve button.

Code: `crates/x-agent` (logic and tests), `src/x_agent.rs` (Hex glue),
`src/rune_ui/agent.rs` (pane), `hex agent` in `src/control/cli.rs`.

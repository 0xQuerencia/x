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
| Computer | "find the latest pull request on GitHub" | a granted browser, using accessibility controls and on-screen evidence |

Every task passes through the same phases, and nothing runs before you approve:

```
idle → listening → transcribing → planning → awaiting_approval → running
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
- This is currently a **native pane tab**, not a globally bound floating
  launcher or a replacement for Hex's dictation shortcut.
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

- This is **not** background clipboard history: X does not poll the clipboard,
  track unrelated copied items, read clipboard images, or watch password-manager
  offers. The user must intentionally capture each snapshot.
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
* **Computer use** requires a per-browser grant and runs a bounded observe, act, verify
  loop. Links and a small set of navigation buttons may be clicked; other buttons and
  consequential controls are refused. The on-screen text sent in prompts redacts email
  addresses, long digit strings, and long tokens. Approval decisions are appended to a
  private local audit file.

## Not done yet

* A native listener shortcut inside Hex (today: bind `hex agent toggle` in your desktop).
* Real Firefox and Chrome profile workflows have not been verified on this build.
* HUD phase indicators for Agent work are not implemented; phase state is visible in the
  Agent pane.
* Permission prompts raised by OpenCode are shown as a stall warning after 120 s of silence,
  not yet as an in-pane approve button.

Code: `crates/x-agent` (logic and tests), `src/x_agent.rs` (Hex glue),
`src/rune_ui/agent.rs` (pane), `hex agent` in `src/control/cli.rs`.

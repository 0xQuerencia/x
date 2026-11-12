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
- On Linux Wayland sessions with `hyprpicker` and `wl-clipboard` installed,
  type `pick color` in Quick to open a **user-initiated** screen pixel selector.
  The selected HEX value is copied to the clipboard. This is optional and
  times out after 45 seconds. Other desktops do not advertise this picker.


- Opening or focusing an app and switching workspace goes through the existing
  Actions planner, showing the same plan and approval controls as spoken input.
- Saved workflows go through their existing validator, preflight and approval
  path. Invalid or conflicting workflow entries are omitted from Quick.
- The workflow list is reused from the manager's short-lived cache; no workflow
  files are reread for every search keystroke.
- A query starting with `=` or `calc ` evaluates bounded arithmetic entirely locally (parentheses, +, -, *, /, %, exponentiation); invalid calculations are refused instead of guessed.
- A HEX or RGB color query (e.g. `color #2563EB` or `rgb(37, 99, 235)`) produces copyable HEX, RGB, HSL, and OKLCH values without a model or screen capture. `Copy` uses the existing Wayland clipboard action, which is not cross-platform yet.
- This is currently a **native pane tab**, not a globally bound floating
  launcher or a replacement for Hex's dictation shortcut.
- The search UI does not run an action automatically on selection; the button
  prepares a plan that can be reviewed in the Commands pane.

Source: `crates/x-agent/src/quick.rs`, `src/rune_ui/agent/quick.rs`,
`src/rune_ui/agent.rs`. Unit tests cover ranking and typed-action routing;
real key focus, UI accessibility, layout and desktop latency remain unverified.

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

# X agent (Labs)

X turns Hex's local speech-to-text into **Actions** (open apps and sites) and **Work**
(hand a goal to OpenCode). Ordinary dictation is untouched: X has its own switch, its own
entry point, and never pastes what you say.

Off by default. Turn it on in Settings › Labs › X agent, or `hex labs on x-agent`.

## Modes

| Mode | What it does | Runs |
| --- | --- | --- |
| Dictate | Hex as before, on Hex's own shortcut | unchanged |
| Actions | "open Firefox and search YouTube for Mars rover" | a fixed set of typed launches |
| Work | "find the bug in this project and fix it" | `opencode run` in a folder you chose |

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

**The Agent shortcut.** Bind `hex agent toggle` (and `hex agent stop`) to a key in your
desktop or compositor, as Hex's Wayland notes suggest for `hex dictation cancel`. Dictation's own
shortcut keeps working, and the two never record at once: X refuses to start while any task
or recording of its own is in flight.

## What X will and won't do

* **Actions** are a closed registry: launch one of seven approved apps, open a public
  `https://` URL, or search one of five sites. A planned step is data, never a command line:
  no shell, no `sh -c`, search text is percent-encoded into one URL argument.
* **Launched is not verified.** X reports "launched"; it does not claim a page loaded.
* **Work never bypasses OpenCode's permissions.** X does not pass `--auto`. The run uses a
  private OpenCode server (`--standalone`) when the installed OpenCode has it. A run only
  counts as completed with exit code 0 **and** no error event; an error event ends the run.
* **Stop really stops**: SIGTERM to the run's process group, SIGKILL after 5 s. X then
  lists files whose git state changed (when the folder is a git repo). Nothing is rolled
  back, and X says so.
* **Local by default.** Speech uses Hex's local model. X sends nothing anywhere itself;
  OpenCode talks to whichever provider you configured.

## Not done yet

* A native listener shortcut inside Hex (today: bind `hex agent toggle` in your desktop).
* The optional local-model planner for phrasing the rules don't cover.
* Desktop automation (typing, clicking). Wayland restricts it; X reports unavailable
  instead of guessing.
* Permission prompts raised by OpenCode are shown as a stall warning after 120 s of silence,
  not yet as an in-pane approve button.

Code: `crates/x-agent` (logic and tests), `src/x_agent.rs` (Hex glue),
`src/rune_ui/agent.rs` (pane), `hex agent` in `src/control/cli.rs`.

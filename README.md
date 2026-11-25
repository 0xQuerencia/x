# X

**Hex, but it can do things.**

I use [Hex](https://github.com/anomalyco/hex) for dictation. It's great at turning speech into text, but sometimes I don't want to type "open Firefox" into a text box. I want Firefox to open.

So I made X.

It's a fork of Hex. The local transcription and regular dictation are still there, with a few extra things for when you want your computer to actually do something.

## The idea

```text
"Remind me to finish the README"
  -> types it into the app I'm using

"X, open Firefox and search YouTube for Mars rovers"
  -> opens the search

"X, switch to workspace three"
  -> switches Hyprland workspaces

"Hey X, open GitHub"
  -> opens GitHub
```

In **Auto** mode, normal speech is just dictation. Start a command with **X** or **Hey X** to make it an action. **You can change the trigger to a word of your choice.** `X` and `Friday` are simply the defaults.

```bash
x agent wake list                 # See your current trigger words
x agent wake set <your-word>      # Replace them with a word you choose
x agent wake add <another-word>   # Or allow more than one
```

Trigger words are single words, up to 24 letters or digits. A few reserved words (such as `hey` and `work`) aren't accepted.

By default, the prefix is recognized after you use your dictation shortcut. Linux also has opt-in hands-free listening, described below; it is off until you enable it.

## What it does

- **Dictation:** Hex's local speech-to-text, pasted into whichever app you're using.
- **Desktop:** Open or switch apps, focus windows, move between Hyprland workspaces, and open sites or searches.
- **Browser:** With permission, inspect accessible page elements, click supported controls, and check what changed.
- **Work:** Review a coding plan, run independent tasks in isolated worktrees, pass committed results to dependent tasks, and run a fixed test suite. Approved coding runs can continue in the background while you use the desktop.
- **Flexible commands:** The optional Decider chooses among the typed actions X supplies. Generative planners are disabled in the current app.

X waits for the finished transcript before acting. Half a sentence won't launch anything.

## Install

X is currently built for **Arch Linux**, with most hands-on testing on **Hyprland / Wayland**. There's no stable packaged release yet.

```bash
sudo pacman -S --needed base-devel git rust openblas wtype nodejs npm

git clone https://github.com/0xQuerencia/x.git
cd x
./scripts/install-arch.sh

systemctl --user enable --now x.service
x labs on x-agent
```

X installs its own app and service without replacing the original Hex. The installer also sets up a pinned [agent-desktop](https://github.com/vercel-labs/agent-desktop) for browser accessibility.

Try a command without speaking:

```bash
x agent say "open firefox"
```

A few useful settings:

```bash
x agent voice auto                 # Dictate normally; commands start with "X"
x agent trust on                   # Skip prompts for allowed basic actions
x agent planner decider-flash      # Choice-only Decider (OpenRouter key required)
x agent planner decider            # Alternative supported Decider model
x agent planner off                # Use local rules only
x agent computer grant chromium    # Let X inspect and operate Chromium
x agent stop --foreground          # Stop foreground work; preserve background runs
x agent stop                       # Emergency Stop for all owned tasks
```

Basic launches don't need a model provider. **Work** needs an installed and configured [OpenCode](https://opencode.ai). Chrome and Chromium need `--force-renderer-accessibility` for browser control.

For local Wayland screenshots, install `grim` and `slurp`, then run `x agent screenshot screen`, `x agent screenshot window`, or `x agent screenshot region`. Remote screenshot reasoning additionally requires `x agent computer remote on` and `x agent computer vision on`; both are separate opt-ins.

For the familiar keyboard flow, bind `x dictation toggle` in your desktop's shortcut settings. **Ctrl+Space** works if nothing else has claimed it.

## Context, voice editing and workflows

Three things that make X feel like part of the desktop instead of a text box.

**It knows what you're looking at.** Highlight an error in an app and say:

```text
"X, search this on GitHub."     -> opens a GitHub search for the selected text
"X, look this up on Stack Overflow."
"X, open this link."            -> opens the selected link (HTTPS only)
"X, copy this."
"X, explain this error."        -> a model explains the selection
"X, focus my editor." / "X, go back to the previous window."
```

"This" is resolved from the real focused window and its real text selection, read through accessibility (AT-SPI) at the moment you ask. Nothing is polled, no keys are watched, and the clipboard is never read. If nothing is selected, the field is a password field, or the app doesn't expose its selection, X says so and does nothing. Before it acts, it checks the window is still there (and, without a prompt, still focused).

**Dictation you can edit.** X remembers what it just typed (a few items, in memory, for 15 minutes) so you can say:

```text
"X, scratch that."  "X, remove the last sentence."  "X, delete the last word."
"X, add a new paragraph."  "X, select the last sentence."  "X, undo that."
"X, make that shorter."  "X, fix the grammar."  "X, make this more casual."
"X, turn this into bullet points."  "X, translate this into English."
"X, replace it."  "X, try again."  "X, keep the original."
```

Edits show a review card with the original, suggestion and a word-level diff, with **Replace / Retry / Cancel** where applicable. Nothing changes until you accept. Right before applying an edit, X refocuses the original window, re-reads the field, and refuses if the text changed while the card was open. "Scratch that" only removes text it can read back and match, and otherwise asks you to select the text. Changes use accessibility and are read back afterwards. Spacing, bullets, and removing the last sentence or word need no model. The generative rewrite examples above need a completion-capable planner; the current native app supports choice-only Decider, so free-form rewrites and translation are unavailable through its planner settings.

**Workflows.** Open **Agent -> Workflows** to build one without touching a file: a name, trigger phrases, and steps (open an app, a terminal in a folder, a folder, a web page, focus or move a window, switch workspace, wait for a window, run an X action, or an OpenCode task). Then:

```text
"X, start coding."
```

Workflows are typed, not scripts: unknown keys and actions are rejected, paths must be inside your home folder (no `..`, no `~/.ssh`), URLs must be public HTTPS, and a trigger can't shadow a built-in command or another workflow. Everything is checked before anything launches, so a missing app or folder fails up front instead of leaving half a setup behind. Windows are watched until they actually appear, moves are verified, Stop works at any point, and the result lists exactly which steps worked. Running one twice reuses windows that are already open. **`x agent workflow undo`** (or "X, undo the workflow") moves windows and workspaces back and says what it couldn't. It never closes a window, since one X opened may hold work you typed since; if a move fails, the remaining changes are kept and saying undo again retries them. Each OpenCode step is checked against the approved workflow right before it runs, so an edited, stale or stopped run is refused.

```bash
x agent workflow list              # with each one's last result
x agent workflow run coding        # plan a run (approve as usual)
x agent workflow import mine.json  # same validation as the editor
x agent context                    # what X can see right now (never the text)
```

### What's actually been tested

| | Result |
|---|---|
| Hyprland / Wayland (Arch), GTK 3 text field | Verified live: context capture (~0.2s), exact selection read, search resolution, verified replace, stale-edit refusal, undo, password field refused |
| Workflow on Hyprland | Verified live: terminal opened in a folder, found by its new window, moved to workspace 10 and confirmed, then undo left the window open on purpose (X never closes windows) |
| Everything else in the three features | Unit and integration tests against fake desktops and fake accessibility |
| Disposable Sway and Xvfb/Openbox sessions | CI exercises actual window operations; this does not establish selection/editing support or physical desktop acceptance |
| KDE Plasma, GNOME, physical Sway/i3 and other X11 desktops | Broader context, workflow and user acceptance checks remain unverified |
| VS Code and other Electron apps | **Not verified.** They only expose text to accessibility when accessibility is switched on, and some editors don't expose it at all. When X can't read the selection it says so and does nothing |
| Qt apps, Firefox/Zen selections | Not tested for selection or editing |

Run the live checks yourself: `cargo test -p x-agent --test live -- --ignored --test-threads=1`. They briefly open a test window and a terminal. The [X release QA checklist](docs/x-release-qa.md) records additional manual application tests, benchmark requirements, and release-quality gates; these are **not** claims that the tests have passed.

**Hands-free wake word.** `hex agent listen on` keeps the microphone open and runs any speech that starts with a wake word ("Friday, start coding", or "Friday" and then the command within 8 seconds) as an X command, planned and approved exactly like a typed one. Everything else is transcribed locally, checked for a wake word and dropped: never typed, logged, stored or sent. It is off by default, steps aside while a dictation is recording, and `hex agent listen off` closes the microphone again. Linux only; it uses the default microphone and the dictation model, so it needs the model loaded. With Trusted Actions on, anything you say aloud that starts with the wake word and matches an allowed action runs without a prompt, so leave Trusted Actions off if others can speak near your microphone.

**Hands-free Smart listening (Linux).** Run `x agent listen smart`, then say
“open Firefox” or “switch to workspace three” without a shortcut or wake word.
Check Agent for the preview and approve it there or with `x agent approve`.
This opt-in keeps the microphone open, uses the loaded local dictation model,
and pauses for normal dictation. It previews single app launches and supported
window controls. Other speech is discarded without pasting or sending it to a
remote planner. A nearby voice or recording can still create a preview. Run
`x agent listen off` to stop and release the microphone.


Known limits: the selection helper needs `python3`, `python-gobject` and `at-spi2-core`. Editing needs fields that support accessibility edits. For apps that expose no accessible selection, "search this", "open this link" and "copy this" can use the last highlighted Wayland primary selection (`wl-paste` required), marked low confidence and always reviewed; that fallback is never used for edits. `open terminal` supports kitty, foot and alacritty. Opening a URL proves launch only. X allows one foreground task and up to four approved background coding/test runs; mixed desktop workflows remain foreground work. Standalone ambiguous window commands offer a task-scoped choice and a fresh review; ambiguity inside a compound command or saved workflow still needs an explicit target.

## Open X Quick from any Linux app

X Quick reuses the app's existing window-control API. Bind the following command
in Hyprland (or another desktop shortcut manager) to open X directly to Quick:

```ini
bind = CTRL, SPACE, exec, hex window show quick
```

This is an opt-in binding: check for conflicts with your dictation hotkey
before enabling it. X agent must be enabled under Labs. It opens the native
Agent pane's Quick tab; it is not yet a separate floating palette. On macOS,
X's agent remains unvalidated and this window destination opens Settings.

## Keep Awake (source builds)

To prevent the OS from idling during a presentation or long task, use
`hex agent awake on --minutes 60`. Check with `hex agent awake status`, and
turn it off using `hex agent awake off`. This is optional, capped at eight
hours, and does not change the dictation state. Some Linux session policies
allow inhibition of idle only, rather than suspending sleep entirely.

## X Quick (source builds)

Use `file report` to search filenames on demand under common home folders
(Desktop, Documents, Downloads, Projects, code and src). Results are local,
limited and never sent to a model. Clicking **Open** requests the default
system file opener; X refuses executable-looking files and reports the launch
as unverified.

For local utilities, type `=2*(3+4)` to calculate or `color #2563EB` to
convert a color to HEX, RGB, HSL and OKLCH. Click Copy to use a result on
supported Wayland desktops (`wl-copy` from `wl-clipboard` is required).
These utilities do not invoke an AI model.


In the Agent pane, open **Quick** to search approved applications, focus an
existing app, switch workspaces or plan a saved workflow without speaking.
Search is local and deterministic. Results use the same typed Actions and
workflow approval paths as the rest of X. Free-form queries also offer an
explicit validated web-search result. Prefix `gh ` or `yt ` to target GitHub
or YouTube, for example; no search launches without selecting it. Quick is an in-app tab, **not** a
system-wide launcher shortcut yet. The public `v0.1.0-alpha.1` binary predates
this work; do not expect it there until a newer build is released and verified.

## Permissions

"Open Firefox" shouldn't mean "run whatever the model wants."

X sticks to a restricted set of actions. You can trust routine launches so they don't ask every time; more sensitive operations still need approval. Browser access is granted separately, password fields are off limits, and OpenCode works within a selected folder with command restrictions.

## Still working on it

It's not a full desktop assistant yet. Browser control works best with Chromium on Hyprland; Firefox, spoken command prefixes, and more complicated page flows still need broader real-world testing. There's no general screenshot-based vision or spoken conversation yet.

Found something broken? [Open an issue](https://github.com/0xQuerencia/x/issues).

## Built on

[Hex](https://github.com/anomalyco/hex) for the app and local transcription, [OpenCode](https://opencode.ai) for coding tasks, and [agent-desktop](https://github.com/vercel-labs/agent-desktop) for browser accessibility.

Thanks to Christopher Langton and everyone who built Hex. X is an independent fork, not an official Hex release.

[MIT licensed](LICENSE).

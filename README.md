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

It's a spoken command prefix, not an always-listening wake-word detector. You still use your dictation shortcut to start speaking.

## What it does

- **Dictation:** Hex's local speech-to-text, pasted into whichever app you're using.
- **Desktop:** Open or switch apps, focus windows, move between Hyprland workspaces, and open sites or searches.
- **Browser:** With permission, inspect accessible page elements, click supported controls, and check what changed.
- **Work:** Give [OpenCode](https://opencode.ai) a coding task, follow its progress, and stop it when you need to.
- **Flexible commands:** An optional model can turn loosely worded requests into a small set of permitted actions. It doesn't get to run arbitrary shell commands.

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
x agent planner gemma4:e2b-it-qat  # Optional local Ollama planner
x agent computer grant chromium    # Let X inspect and operate Chromium
x agent stop                       # Stop the current task
```

Basic launches don't need a model provider. **Work** needs an installed and configured [OpenCode](https://opencode.ai). Chrome and Chromium need `--force-renderer-accessibility` for browser control.

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

Rewrites show a small review card, original, suggestion and a word-level diff, with **Replace / Retry / Cancel**. Nothing changes until you accept. Right before it applies, X refocuses the original window, re-reads the field, and refuses if the text changed while the card was open. "Scratch that" never sends Ctrl+Z: it only removes text it can read back and match, and otherwise asks you to select the text. Changes are made through accessibility, not keystrokes, and read back afterwards. Spacing, bullets, and removing the last sentence or word need no model. Rewrites use your planner; a remote model (Gemini, an OpenAI-compatible endpoint) always asks first, each time, before any text leaves your machine, and prompts are never stored.

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
| KDE Plasma, GNOME, Sway/i3, X11-only | **Not tested.** The window backends exist, but context and workflows are unverified there |
| VS Code and other Electron apps | **Not verified.** They only expose text to accessibility when accessibility is switched on, and some editors don't expose it at all. When X can't read the selection it says so and does nothing |
| Qt apps, Firefox/Zen selections | Not tested for selection or editing |

Run the live checks yourself: `cargo test -p x-agent --test live -- --ignored --test-threads=1`. They briefly open a test window and a terminal. The [X release QA checklist](docs/x-release-qa.md) records additional manual application tests, benchmark requirements, and release-quality gates; these are **not** claims that the tests have passed.

**Hands-free wake word.** `hex agent listen on` keeps the microphone open and runs any speech that starts with a wake word ("Friday, start coding", or "Friday" and then the command within 8 seconds) as an X command, planned and approved exactly like a typed one. Everything else is transcribed locally, checked for a wake word and dropped: never typed, logged, stored or sent. It is off by default, steps aside while a dictation is recording, and `hex agent listen off` closes the microphone again. Linux only; it uses the default microphone and the dictation model, so it needs the model loaded. With Trusted Actions on, anything you say aloud that starts with the wake word and matches an allowed action runs without a prompt, so leave Trusted Actions off if others can speak near your microphone.

Known limits: needs `python3` with `python-gobject` and `at-spi2-core` for the selection helper; editing only works in fields that accept edits through accessibility (others are refused rather than typed into); for apps that expose no text to accessibility (browsers, some Electron apps) "search this", "open this link" and "copy this" fall back to the last text you highlighted (the Wayland primary selection, needs `wl-paste`), marked low confidence and always shown for approval even with Trusted Actions on, and never used for edits; `open terminal` supports kitty, foot and alacritty; opening a URL can't be verified; one task (or workflow) at a time; ambiguous windows in a workflow step fail with a message and a `pick` option rather than a chooser.

## X Quick (source builds)

In the Agent pane, open **Quick** to search approved applications, focus an
existing app, switch workspaces or plan a saved workflow without speaking.
Search is local and deterministic. Results use the same typed Actions and
workflow approval paths as the rest of X. Quick is an in-app tab, **not** a
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

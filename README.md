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

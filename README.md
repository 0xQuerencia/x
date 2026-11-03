# X

X is a local-first voice and agent fork of [Hex](https://github.com/anomalyco/hex) for Linux (built and tested on Arch/Hyprland). Hex's local speech-to-text and dictation are kept as they were. X adds three things on top, all off until you turn on the Labs switch `x-agent`.

- **Actions**: say "Friday, open Firefox and search YouTube for Mars rover". X plans it with fixed rules first and a model for anything else, then launches apps, opens sites, switches workspaces and windows, types into a window it just opened, or starts an OpenCode session in a temporary folder.
- **Work**: hand a goal and a project folder to [OpenCode](https://opencode.ai). X streams its events, blocks commands outside a policy (you can allow one for a single rerun), and reports the files that changed. Stop works at any time.
- **Computer use**: operate a browser window through its accessibility tree. X observes, picks one step, acts, re-observes and only reports done when it can quote text that is on screen now. It is limited to browsers you grant, and never reads password fields.

Normal speech is typed as dictation as usual. Only speech that starts with "Friday" (or "hey Friday") is treated as a command.

## Install (Arch Linux)

```sh
sudo pacman -S --needed base-devel git rust openblas wtype nodejs npm
git clone https://github.com/0xQuerencia/x && cd x
./scripts/install-arch.sh
systemctl --user enable --now x.service
x labs on x-agent
```

The installer builds with `X_BUILD=1`, which gives X its own identity (data in `~/.local/share/x`, config in `~/.config/x`, `x.service`, app id `anomaly.x`), so it can sit next to an official Hex install. It also installs a pinned [agent-desktop](https://github.com/vercel-labs/agent-desktop) for reading accessibility trees. OpenCode must be on `PATH` for Work.

## Use

```sh
x agent voice auto                  # only "Friday ..." speech becomes a command
x agent trust on                    # run simple launches without a prompt
x agent planner gemma4:e2b-it-qat   # local Ollama model; also gemini or openai:<model>
x agent say "open firefox"          # same as speaking it
x agent computer grant chromium     # let X read and operate that browser
x agent stop
```

Bind your dictation key to `x dictation toggle`. Chromium and Chrome need `--force-renderer-accessibility` for computer use.

## Safety

- Every step a model proposes must fit a closed set and pass validation. Nothing is run through a shell.
- Screen content is treated as untrusted data, never as instructions. On-screen text goes only to a model on your machine unless you run `x agent computer remote on`.
- Sending, buying, deleting and signing in are refused. Secret fields are never typed into.
- Work never runs with `--auto`, and is pinned to the chosen folder.

## Status

Verified on one machine (Arch, Hyprland, Wayland): Actions, Work with a real OpenCode run, and computer use against a disposable Chromium profile (click and verify, navigate and verify). Not verified: spoken wake-word commands end to end, Firefox, screenshots and vision (not built). Browser control relies on keyboard input that another window can interrupt if it steals focus.

## Credits

X is built on Hex by Christopher Langton and contributors, under the MIT licence in `LICENSE`.

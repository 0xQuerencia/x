# X release quality gate (0.2 alpha)

This is an **X-specific** smoke-test plan. It is not a claim of completed testing.
Do not publish a new alpha until the actual release artifact, not merely the
working tree, has passed the relevant tests.

## Automated baseline

Run on an Arch Linux workstation with Rust installed:

```sh
cargo fmt --all -- --check
cargo test -p x-agent
cargo clippy --workspace --all-targets -- -D warnings
```

The full Hex workspace has separate upstream requirements; run
`cargo test --workspace --no-fail-fast` before a release, recording any
pre-existing, reproducible failures separately.

The ignored live test suite **changes the active desktop**, so run it only
on an expendable Hyprland test session:

```sh
cargo test -p x-agent --test live -- --ignored --test-threads=1
```

For Rune UI changes, regenerate and inspect golden screenshots on Linux,
using the repository's `AGENTS.md` instructions before merging.

## macOS source preview (unsigned)

`x-macos-preview-<commit>` prereleases, when the macOS arm64 runner
successfully compiles and tests X, contain the **bare executable**
(`x-macos-arm64`) and a distinct unsigned **X Preview.app ZIP** called
`x-macos-preview-arm64.zip`, together with `BUILD_INFO.json`,
`SHA256SUMS` and notes. The bundle ID is
`art.querencia.x.preview`, its executable matches the bare X binary
byte-for-byte, and its Info.plist explains why microphone access may
be requested. The build sets X-specific data and display identities
and does not use upstream Hex signing or release infrastructure.

The `.app` is **not notarized, Developer ID signed, a DMG, an installer
or a verified macOS distribution**. Gatekeeper may refuse it; don't
disable system protections to make an untrusted preview run. Real
microphone, shortcut, accessibility, window-control, screen-recording,
quarantine and process-lifecycle behavior are still unverified. CI
bundle structure checks alone do not establish a functional Mac app.

## Preview artifact provenance

Unsigned Linux and Windows previews include `BUILD_INFO.json` and SHA256
checksums in `SHA256SUMS`. The manifest records the exact source commit,
build platform, compiler, target, artifact byte sizes and hashes. CI checks it
on the builder and again after downloading the artifact for prerelease
publication. To check an extracted preview alongside a copy of the source:

```sh
python3 scripts/write-preview-manifest.py verify --directory dist \
  --source-sha <40-character-source-commit> --platform linux
```

Use `--platform windows` when verifying a Windows bundle. All manifest
records and hashes must match. **These unsigned checksums are not a signature
or proof that a downloaded artifact originated with a trusted publisher.**
Validate the release destination and trusted source SHA independently.
A manifest is not a substitute for fresh-install acceptance, real-device QA,
Windows signing or published software provenance attestations.

## Arch upgrade and rollback

The Arch installer stages its replacement executable under
`~/.local/opt/x/` and atomically renames it into place. It preserves the
previous executable as `hex.previous` and refuses to take over an unrelated
`~/.local/bin/x` command. This protects the binary replacement only, not
every setting or dependency upgrade.

If the new binary fails to start, use the explicit recovery command from
the checkout:

```sh
bash scripts/rollback-arch-x.sh
```

This restores the saved executable without changing X workflows or settings.
Recheck `x agent doctor` and live service behavior after rollback. A fresh
install has no previous executable to restore. The installer and rollback
still need a real clean-account acceptance run before release.

A macOS release also requires the `screen_permission` and `screenshot`
cases to be recorded as passing: capture the main display under an explicit
local request, revoke Screen Recording, verify no protected content is leaked
and check that unsupported window/region capture refuses. Do not mark the
permission test passing merely because a screenshot file was created; macOS
may return blank content when recording access is denied.

## Private local physical-acceptance record

A release gate cannot infer live microphone, UI, window identity or installer
correctness from GitHub Actions. The portable `scripts/x-qa-gate.py` tool keeps
an **operator-reported checklist** per target platform and exact source SHA.
It never records usernames, private window titles, transcriptions, API keys,
screenshots or arbitrary free-form notes.

For example, on an actual Windows 11 test machine:

```powershell
python scripts/x-qa-gate.py init --file x-windows-qa.json --platform windows11 --source-sha <40-character-commit>
python scripts/x-qa-gate.py record --file x-windows-qa.json --case microphone --result pass
python scripts/x-qa-gate.py record --file x-windows-qa.json --case hotkey --result pass
python scripts/x-qa-gate.py check --file x-windows-qa.json
```

Use `hyprland`, `kde`, `windows11`, or `macos` as the platform; all
available case names are listed in `scripts/x-qa-gate.py`. Record `fail` or
`unsupported` instead of `pass` when appropriate. The check exits nonzero
for **any** missing, failed or unsupported required case. It never tags or
releases anything. Records are private local files, with owner-only file
permissions on Unix; keep them out of public issue logs.

An all-pass record is **not independent verification**: someone must actually
perform each test and inspect the exact release artifact. Build signing,
distribution and deployment approvals remain separate release requirements.
The Python CI tests only verify the schema and fail-closed gate logic, not
any physical X functionality.

## X helper installation ownership

The Arch source installer also checks the optional local `agent-desktop`
helper directory and the `~/.local/bin/agent-desktop` command before running
npm. It refuses symlinked or unowned helper directories, unrecognized manifests
or an existing unrelated command. A preexisting user-owned helper is accepted
only when its manifest matches the pinned X helper dependency; the installer
never silently takes over another tool. The isolated ownership tests run in
`x-arch-install.yml`. This remains **installer CI evidence**, not a fresh
physical clean-account installation.

## Current X master-plan acceptance gate

These checks are still **not evidence of a successful live run** until an
operator records the actual results, machine and commit. The merged Rust unit
tests do not substitute for any of the following.

1. On a clean second person's Hyprland account, run the current installer,
   `hex agent doctor`, service start/stop, and an explicit free-Decider setup
   with `hex agent key set`. Run 20 real spoken commands, including installed
   Flatpak apps, loose window phrasings, two matching windows, an Undo, network
   offline behavior and a wake-word injection attempt. Require at least
   **18 correct on first attempt**, no destructive action without its required
   approval, a voice response to every ambiguity and a green doctor report.
2. Execute both ignored live Hyprland suites only in a disposable session:

   ```sh
   cargo test --locked -p x-agent --test live -- --ignored --test-threads=1 --nocapture
   cargo test --locked -p x-agent -- --ignored live_hyprland --test-threads=1
   ```

3. Measure 50 local CLI round trips using
   `scripts/smoke-x/bench.sh hex 50`, and separately measure real
   network Decider and action/verify p95. The bench script intentionally
   **does not** measure model, microphone or actual action latency. Do not
   claim the proposed 150ms/1.8s figures without physical measurements.
4. Test the headless Sway and X11 fixtures in CI, and real desktops where
   possible. KDE Plasma 6/KWin and GNOME Wayland must be reported as
   **experimental or unsupported** until their dedicated live tests pass.
   GNOME's companion extension is an owner decision, not an implied deliverable.
5. Confirm the user control socket is user-owned and mode 0600, its parent
   directory is 0700, and no logs contain voice transcripts, window titles,
   selected text or the OpenRouter key. Check this on an installed service,
   not only by reading the source.
6. Validate that the real release artifact retains privacy settings, grants,
   stored workflows and rollback semantics, and that the published website
   does not overstate the supported Linux desktops.

Unresolved: a second person's live Phase 1 exit run, user-machine performance
numbers, real KDE Wayland smoke, any GNOME Wayland companion extension, and
owner authorization for tags/AUR publishing.

## Physical application matrix

Use a fresh install and real microphone, not injected transcript strings.
For each attempted operation record: OS/compositor, app/version, source of
selection, model, expected result, actual result, and retry outcome.

| Scenario | Evidence required |
| --- | --- |
| Fresh Arch/Hyprland install | no existing Hex overwritten; X service starts; model downloads; shortcut toggles |
| GTK editable field | dictation, scratch that, grammar preview/accept, undo, stale-edit refusal |
| Qt editable field | same operations; explicitly note if accessibility is unsupported |
| VS Code/Electron | selected error context, no-selection fallback, no unsafe editing |
| Chromium and Firefox | basic browser navigation, inaccessible element failure, permission grant/revoke |
| Two editor windows | workflow focus/move refuses without a choice; existing pick is retained after editing |
| Window closes after selection | stale pick refuses; other windows remain untouched |
| Workflow with a late invalid target | preflight rejects before any early launch |
| OpenCode task | allowed/rejected command, Stop, changed-file summary, clean failure |
| Wake listener | opt-in, ordinary speech discarded, microphone closes on off, untrusted speaker cannot trigger sensitive actions |
| X shortcut conflict | conflicting desktop binding is reported, not silently swallowed |

Do **not** mark a scenario passing on the basis of a fake desktop test. If
the compositor, app or accessibility bridge is unsupported, record the result
as unsupported rather than passing.

## Performance baselines

Record at least 30 warm runs per test on the same machine, report median
and p95, and distinguish cold-model from warm-model execution:

- key press to visible capture HUD
- release to final transcript
- final transcript to focused-app insertion
- context selection retrieval, with source and confidence
- command to first verified desktop action
- full workflow completion, including UI wait time
- idle RSS and CPU usage

Collect a baseline first; choose regression limits from measured distributions.
Do not claim a fixed startup or action latency before measurements exist.
Redact prompts, selected text and private paths from shared diagnostics.

## Workflow window-selection contract

Focus and Move steps use `pick` as a **1-based ordinal** in the freshly sorted
list of matching windows (workspace, then ID). Leaving it unset means automatic
selection **only when exactly one** window matches.

The editor preserves this value and exposes it, but it is not a permanent
window identity. Windows may disappear, appear or reorder after the workflow
is saved. A saved out-of-range pick must be refused; never silently redirect
it to a different window. Preflight should catch currently stale or ambiguous
targets before side effects; execution must still recheck.

X's interactive ambiguity chooser now retains and revalidates exact window
identity (ID, PID and class) at choice time. Saved workflow `pick` values
remain **ordinals**, not permanent identities: do not conflate the two.
The live two-window chooser and late-window-open tests remain a release gate.

## Release checklist

- [ ] Automated checks pass with a recorded commit SHA and environment
- [ ] Rune golden images updated and inspected if UI changed
- [ ] Real microphone + desktop matrix recorded (including unsupported cases)
- [ ] Linux installer and dependencies checked on a clean account
- [ ] Downloaded artifact SHA-256 verified
- [ ] Service start/stop, crash recovery, cancellation and rollback verified
- [ ] Website feature claims match the **released** artifact
- [ ] Release page lists known unsupported platforms and apps
- [ ] README examples and commands verified against that same artifact

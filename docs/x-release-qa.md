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

Future improvement: replace ordinal picks with a native runtime chooser that
pins and revalidates an exact window reference (ID, PID and class). Do not
claim this exists until it is implemented and live-tested.

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

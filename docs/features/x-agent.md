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
| **Autonomous** | Low-risk desktop actions start automatically. Typing, closing a window, operating an app, opening a website, sending a search to a website, and Work each require approval. |

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
keeps the same saved setting. A fresh install starts in **Default**. An
existing Trusted Actions setting keeps automatic execution for low-risk actions.
X shows the tighter approval rules once after upgrading.

A task runs through the same phases, but Autonomous may skip
`awaiting_approval` on permitted tasks:

```
idle → listening → transcribing → planning → [approval if required] → running
     → completed | failed | cancelled
```

## Experimental Gemini Live talkback

On Linux, enable **Gemini talkback** in Labs and run `hex agent talkback`
from a visible terminal that reports key press and release. Supply your
project's key via `X_GEMINI_LIVE_API_KEY`. Talkback asks for consent for each
terminal session. Hold Space to record locally and release to send only that
clip to Google. Space interrupts the current reply. Escape or leaving the
terminal cancels local recording and playback. Q exits.

Google may use unpaid inputs and responses to improve its products, and
human reviewers may process them. Keep talkback free of sensitive,
confidential, or personal information. Project quotas apply to unpaid usage.
[Gemini terms](https://aistudio.google.com/docs/terms#unpaid-services) ·
[Gemini rate limits](https://aistudio.google.com/docs/rate-limits)

The default model is `gemini-3.8-live`. `--model` selects another Gemini Live
model. A recording is capped at ten seconds and opens one short connection
on release. Connection expiry, GoAway, quota and network errors return to
quiet mode. A later key press can start a new turn. The command handles
microphone audio and spoken responses only. Dictation and desktop actions
use their existing paths when talkback is off or unavailable.
[Gemini Live model](https://aistudio.google.com/docs/models/gemini-3.8-live) ·
[Live session limits](https://aistudio.google.com/docs/live-api/session-management)

## Using it

### What execution results prove

Actions retain typed native evidence through the service boundary. A launch or
keyboard dispatch is `launched`; it does not prove that the requested page,
field or application effect occurred. Native window, system and clipboard
adapters report `confirmed` only after their own postcondition checks. An owned
screenshot write confirms that file creation. Window enumeration and selected
accessibility text are `observed`, which does not establish an unrelated UI goal.
A failed native readback is `unverified` and stops the action sequence. Display
messages and window titles never determine these outcomes.

Status JSON adds optional `steps[].execution` (`state`, `message`, and typed
`evidence`) and `action_outcome`. The latter distinguishes `confirmed`,
`launched`, `observed`, `partial`, `unverified`, `failed` and `cancelled` for
terminal Actions tasks. Existing phase/status fields remain compatible:
`completed` means execution ended, and the typed outcome says what was proved.
An observation keeps the conservative legacy `launched` step status. Mixed
confirmed and unverified work is not projected as fully confirmed. Stop or
failure overrides earlier proof in the terminal task projection.

`Action::execute()` and legacy launch adapters retain their string API, but
those strings provide no native proof. Trusted adapters use
`Action::execute_outcome()` and the typed Launcher method. Regression tests
cover successful readback with `still open` in a document title, failed
readback with different wording, unsupported proof, legacy JSON parsing and
mixed task outcomes.

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

App launches use exact installed application names, executable identities or
desktop-entry IDs. Desktop search keywords can help identify an already open
window, but do not name an application to launch. For example, a terminal's
`bash` keyword and a settings app's `agent` keyword cannot claim those launch
requests or shadow a saved workflow. The catalog regression uses both desktop
entries and keeps exact-name launches available.

Local compound commands keep up to five ordered steps, including app launches,
window operations and volume/mute controls. Quote text or a search query when it
contains action words: `open terminal then type "hello and open Firefox"` types
that literal phrase after the normal focus and approval checks. Words and spaces
inside straight or curly quotes are preserved; an incomplete quote or an invalid
step rejects the entire plan. A browser followed by a quoted search keeps that
browser as its destination. Unquoted standalone text/search payloads keep their
existing literal behavior. Parser regressions cover quoted action words,
apostrophes, Unicode, bounds, negation and invalid later steps; they do not replace
physical typing or focus acceptance.

**Voice routing.** The Agent pane controls the dictation shortcut's mode: Dictate, Actions,
Work, Auto, or Smart. Auto only routes speech beginning with Friday or X. Smart asks the
planner whether speech is a command. A planner error is reported and consumed; it is not
passed to the focused app as dictation. Escape cancels an active Agent task while the Agent
pane is selected.

On Linux, use `x agent voice smart` with your usual dictation shortcut. Try
“open Firefox” without a wake word. Smart checks the original transcript
before dictation rewrites. Recognized commands go through the normal approval
policy. Speech classified as dictation goes through your usual processing and
paste. With the planner off, Smart only recognizes built-in commands. A remote
planner receives transcripts that the local rules could not classify, including
ordinary dictation. Smart only runs after you use the shortcut.

**The Agent shortcut.** Bind `hex agent toggle` (and `hex agent stop`) to a key in your
desktop or compositor. Dictation's own shortcut keeps working, and the two never record at
once: X refuses to start while any task or recording of its own is in flight. After
Stop, an outstanding transcription request must return before another Agent
recording starts. Its late transcript is discarded. Stop also blocks a
finished transcript from creating a new typed desktop plan while it is being
prepared. Transcription activity
reports status without adding the utterance to the event stream. The exact
submitted command is still visible in the task preview.

The [isolated Hyprland voice test](../../scripts/smoke-x/hyprland-voice.sh)
supplies a finished transcript at the speech-to-text boundary. It exercises
the Agent voice service and real compositor readback, including approval and
Stop. This check begins after speech recognition and does not use a microphone.

### Register a project

```sh
hex agent projects register "My project" ~/code/project
hex agent projects list
hex agent projects alias-add <project-id> "My alias"
hex agent projects use "My alias"
hex agent projects alias-remove <project-id> "My alias"
hex agent projects forget <project-id>
```

Each explicitly chosen folder gets a stable local ID. X resolves links and
requires the result to stay inside your home, outside private configuration
trees. For a Git checkout, X records the local Git common directory so linked
worktrees share the same repository identity. Registering a folder again
returns its existing entry. Forget removes only the registry entry.

Names and aliases resolve by exact normalized match, never fuzzy matching.
When multiple projects share the same name or alias, select by the stable ID.
Selecting a project validates its directory again and sets the Work workspace.
A selected project is remembered between X restarts; it never bypasses Work approval.
Aliases are local metadata. Forget removes the registry and saved session metadata,
not the project files.

In Actions or Smart voice mode, say “Friday, list projects,” “find project
Chamber,” “switch to project Chamber,” or “open my Chamber project.”
Listing and finding read the local registry. Switching and requesting the
file manager wait for approval with the exact project and directory in the
preview. Opening uses the system file manager and reports the launch as
unverified.

“List saved sessions” displays remembered IDs for the selected project with
unverified availability. “Resume OpenCode session ses_…” previews an exact
session selection. “Resume session one” also accepts an ordinal from the
current, unexpired OpenCode listing. Approval confirms the session with
the provider before selecting it. A separate Work task sends a goal.

The registry tests cover restart, linked Git worktrees, private paths,
symlink escapes, ambiguity, and preservation of files when forgetting a project.

### Explicit context attachments

X's spoken selection-to-session handoff now wraps the selected excerpt in a typed,
bounded **untrusted context bundle**. It carries the source, capture time, scope
and redaction state. Password fields and unverified primary selections are
refused by the existing accessibility guard; attached text cannot grant
permissions or become an X desktop action.

The same bundle contract supports bounded user-chosen log and Git diff excerpts,
project-scoped file-line references and query-stripped browser page metadata
with credential-like URL paths and titles redacted.
Screenshot references require explicit screen permission; **image bytes are not
automatically captured or sent**. Selected text and explicit project-file
excerpts are wired into the command surface. Other sources still need separate
permissioned connectors rather than ad-hoc string interpolation.



The CLI additionally accepts `hex agent send-project-file src/main.rs --line 5 --count 10`
after explicitly selecting a project and an OpenCode session. X reads only the
chosen 1–20 UTF-8 lines, refuses hidden/out-of-project paths, redacts common
credential lines, and caps the excerpt to 900 bytes. The handoff uses the normal
**Work approval** and selection-only harness mode; it does not grant file edits
or scan surrounding files. To share **manually entered** browser metadata, use
`hex agent send-browser-page https://example.org/docs?token=private --title "Docs"`
after selecting a project and session. X strips query/fragment, redacts known
credential patterns in the path and title and asks for Work approval. This is
**not an automated browser-tab connection**: it never reads the current browser,
fetches the page or forwards page contents. Direct tab and screen capture
still require separately permissioned adapters.

### Owner-approved isolated project tests

`x agent workflow test-project <registered-project-id> <suite>` plans a bounded,
non-model-selected test on a **new isolated Git worktree** at the approved
project's current commit. Suite is one of `cargo-workspace`, `cargo-agent`,
`npm`, `pnpm`, `pytest`, or `go`. Tests execute arbitrary repository scripts **with the user's normal OS permissions**.
An isolated Git worktree protects the primary checkout from ordinary writes,
but is **not a security sandbox**: untrusted scripts can read the home directory,
use the network or alter files elsewhere. Tests therefore **never run
automatically**, even in Autonomous mode.
Review and approve the exact project, commit, and preset from the Agent pane or
`x agent approve <id>`. Stop cancels the supervised test process. A private
one-use, exact-scope grant is consumed immediately before test execution;
a stale project or changed commit is refused. The resulting worktree is left
for inspection; it is **not merged or deleted automatically**. A green test
exit only verifies that preset's process result, not the project as a whole.

### Registered projects in Rune Work

In Rune’s Agent → Work pane, **Browse projects** lists saved names from X's
existing project registry, five per page. Selecting one uses the same
validated registered-root selection as `hex agent projects use`; a stale,
ambiguous or moved project is refused. The selection updates the shown
workspace for future Work tasks, but does not run code, grant a coding agent
access, or bypass normal approval. Project paths are shown only to the
local user, never sent to a model by browsing.

Project registration and alias editing remain in the existing CLI. Work
must be idle before a project can be switched. A physical Rune smoke
test is still needed to verify keyboard focus, layout and pagination.

### Selecting existing OpenCode sessions in Rune

The Work pane's **Find sessions** button asks the existing local OpenCode
service for sessions scoped to the currently selected project. Up to six
recent sessions are shown for selection; the CLI still lists all sessions.
Selecting a listed session performs an **exact fresh provider lookup**
before X records it, and refuses moved or disappeared sessions. Changing the
project clears the Rune session list so stale selections are not implied
valid. No session prompt or transcript is displayed, no job is started,
and future Work sends still need approval.

The lookup may take a few seconds and needs a running local OpenCode
service. This is not a general process scanner, background session monitor,
or automatic cross-project session switch. Real keyboard and layout
acceptance in Rune remains pending.

### Owner-supplied browser metadata in Rune Work

Rune's Work pane offers **Review page handoff** for a URL and optional title
typed by the owner. The same `plan_browser_page_to_session` validation and
redaction used by the CLI binds this metadata to the currently selected
OpenCode session with a new one-use Work approval. Browsing the project list
or entering the URL does not send data, open a browser, read tabs or start
code. The actual browser page body, cookies, screenshots and browser state
are **not** captured. Confirm privacy, keyboard focus and review text on
the actual Rune UI before treating the flow as release-ready.

### Share selected browser text

With a project and an OpenCode session selected, use a desktop keybinding for
`hex agent send-browser-selection` while the browser has focus and text is
selected. X requires a grant for that browser and `hex agent computer remote on`
before reading through accessibility. It reads only from the application's
active accessibility window and refuses when that window is ambiguous. It
rejects missing or oversized selections and leaves the clipboard alone.

Review the redacted excerpt and selected session in the Work preview.
Approval rechecks the browser grant and window identity before handing the
excerpt to OpenCode under the selection-only execution policy. Focus can
move to X for review.
This command shares selected text only. It leaves tabs, console logs, and
screenshots untouched.

### Read-only recovery claim history

`hex agent workflow claims` displays every locally recorded coding job's run
ID, node ID, attempt, outcome and whether evidence was stored, **without**
printing prompts, task fingerprints or provider evidence identifiers. It never
starts a harness, issues grants, or retries jobs. X records a structured harness
acknowledgement as it arrives; the claims view shows when that evidence blocks
retry. Receipt and completion are separate. `hex agent workflow uncertain`
includes whether delivery was observed for each job that still needs an
outcome. When delivery is unknown, any retry requires independent proof of
non-delivery and a new approval. That provider check is still pending.

### Approved Git fast-forward integration (conservative)

After inspecting an **X-owned Git worktree** and committing its changes, the
owner can request a merge preview with
`hex agent integrate-fast-forward <registered-project-id> <run-uuid> <task-id> <start-commit>`.
The IDs and original 40/64-character commit must identify an existing worktree
created by X. X verifies the registered Git identity, owner-generated worktree
path, uncommitted state, branch ancestry, exact primary and task commits and
changed-file list before offering **Approve and run** in Rune (or `hex agent
approve <id>`). The approval is one-use and scoped to the reviewed commits.

Only **clean, conflict-free Git fast-forwards** are supported; diverged branches,
dirty checkouts, stale approvals, unsafe worktree identity and truncated change
lists are refused without edits. X uses `git merge --ff-only`, never push,
force, reset, checkout arbitrary model paths, resolve conflicts or delete the
task checkout. Once Git actually starts, Stop cannot safely interrupt a branch
update; Stop can cancel the pending approval or stop before the Git call.
Inspect the primary checkout manually if Git reports an incomplete operation.

### Natural project voice selection

X recognizes deterministic registered-project phrases such as `work on
project X`, `let's work on project X`, `switch back to project X`,
`find my project X`, and `show my projects`. They use the same exact
registered name/alias resolution as `use project X`: ambiguous names
require an ID; unknown names do not become arbitrary filesystem paths,
and changing the selected project still enters normal approval.

Parsing is local: no generative planner or paid model is needed.
Unrestricted requests to invent or register projects are not supported.

### Owner-selected terminal output handoff

X can attach a **bounded excerpt the user deliberately supplies** from a
terminal to the currently selected OpenCode session. For example, after
selecting a project and an existing OpenCode session:

```sh
hex agent send-terminal-output "cargo test: 1 failed; 98 passed"
cargo test 2>&1 | hex agent send-terminal-output --stdin
```

The second command runs `cargo test` **in your shell**, not inside X. X
reads at most 1201 bytes from standard input, refuses excerpts above 1200
bytes, redacts common credential-like lines and labels the content as
`terminal_output`, untrusted project evidence. Output with raw escape
control characters is refused. It never watches a PTY, grabs terminal
scrollback, runs a shell or adds execution privileges. Sending the excerpt
to a selected OpenCode session still requires the normal **explicit Work
approval**, including in Autonomous mode, and uses selection-only
tools-disabled access. If the CLI is waiting for terminal input, finish the
pipe or cancel it; X only reads stdin when `--stdin` was requested.

Do not pipe secrets or full build logs blindly. Pattern redaction is best
effort and is **not** a general data-loss prevention system; inspect the
selected excerpt before approving it.

### Workflow preview and dry run

Run `hex agent workflow preview <id>` to validate a saved workflow or v2/v3/v4
graph and print its ordered nodes, dependencies and actions as JSON. The
response explicitly says `executed: false` and `authorization:
not_requested`; no desktop action, coding harness, approval or merge is
triggered by this command.

Use **Dry run** in the Workflows list or editor, or run
`hex agent workflow dry-run <id>` to validate a sequential workflow and check
its current desktop prerequisites. This works with Autonomous mode on and
leaves any existing task untouched. The editor can check unsaved changes. The
CLI returns the ordered steps and either a passing preflight or a refusal with
the missing prerequisites. Running the workflow checks the prerequisites again.

`workflow run` reviews a sequential workflow;
`workflow run-graph` reviews a supported typed graph before execution.
Saved runs that open a website, use a spoken action step, close a window, or
run OpenCode require approval in Autonomous mode too. Hands-free speech can
prepare a saved run for approval. A sequential step with `requires` waits
for a verified predecessor. An unverified launch skips dependent steps.
Sequential runs allow at most 20 steps and stop when they reach three minutes.

### Verified literal text in a sequential workflow

In **Agent → Workflows**, add an **Open app** step set to `text-editor`, then
add **Insert text into new editor** and enter that step's ID and the literal
line. The saved JSON uses `{"action":"type_text","opened_by":"step-1","text":"A local note"}`.
Use the workflow preview to review the saved steps, then start the run and
approve the exact text. A line can contain up to 300 characters.

The Hyprland adapter uses AT-SPI to insert and read back the line in the
focused editor window opened by this run. Reused windows, ambiguous focus,
protected fields and failed readback stop the typing step. The result includes
verification status. The local workflow file and approval preview show the
literal text. Run summaries and activity omit it.

The disposable Hyprland test uses a GTK editor fixture. Other desktop
backends return unsupported for this operation. Graph runs continue to
refuse this sequential step.

### Workflow DAG schema (development)

The new `workflows::dag` module validates versioned graph JSON, detects
cycles, unknown dependencies and duplicate edges, and computes a deterministic
topological order. It migrates existing sequential workflow files in memory:
`after` means ordering only, while `requires` still requires success.
That distinction preserves legacy `on_failure: continue` behavior.

`Store::get_graph` reads both formats. The standard workflow runner
continues to execute sequential desktop workflows.

### Workflow permission review (development)

Run `hex agent workflow permissions <id>` to inspect the operations in a
validated workflow graph. The response identifies desktop and coding-harness
nodes, distinguishes service approval from one-use task-scoped grants, and
shows the registered project identity requested by each harness node.
It does not echo private coding goals, issue permissions, start a harness or
execute workflow steps.

### DAG scheduler contract (development)

A bounded coordinator is available behind the new graph contract. It requires an
executor to approve **every node** before launch, checks authorization again
on dispatch, limits concurrent jobs (at most four), and propagates Stop to
child cancellation tokens. Without worktree isolation, mutating harness jobs
for the same project run one at a time; desktop jobs also share one resource
lock. Unverified outputs never unlock success-dependent tasks.

Approved coding graphs use two workers and require a verified, owned Git
worktree for each job.
The task steps reflect scheduler dispatch and each node's terminal outcome
while other jobs are still running. A skipped dependency never appears as
running. These states describe job execution, not a percentage of code work.
The scheduler joins owned workers before releasing their resource locks and
reports a worker spawn failure or panic as Failed. A stop-on-failure node
prevents new dispatch but lets already-running siblings finish. Global Stop
requests cancellation of every child; workers must cooperate with that token,
and X does not claim that an unresponsive worker has been terminated.

In the Agent panel, each queued or running coding job has its own Stop
button. It requests cancellation of that exact reviewed job, not its siblings.
A job waiting to start will not launch; a running job must acknowledge Stop
and exit before its resource is released. Success/proof-dependent jobs remain
blocked when their prerequisite is cancelled. Ordering-only edges can still
release independent work. Global Stop still cancels the entire graph. A
partially cancelled graph is not reported as fully completed.

### Approved Git worktree isolation (development)

The new worktree manager can allocate a detached Git checkout at a verified
commit into an owner-selected visible directory under the home folder. It
verifies the repository identity, never changes the original checkout and
refuses duplicate task paths. Git hooks are disabled for these operations;
Git checkout configuration and filters still require explicit owner consent.

Cleanup **requires a new owner approval**, rechecks ownership and Git identity,
refuses tracked/untracked/ignored changes and never uses `--force`.
The coding graph keeps each worktree for inspection after the run. Before
launch, it rechecks both the pinned starting commit and clean worktree files;
queued work does not silently inherit unapproved edits. Merging and cleanup
require their own explicit action.

### Crash-safe DAG task claims (development)

The workflow recovery journal stores run/node identities, a request
fingerprint, attempt count, exact preparation identities and evidence references. It does **not** store
prompts or tokens. It atomically claims a node before delivery; if X loses
contact after the claim, restart reports `RequiresReconciliation` rather than
silently sending the same work again. A terminal result is immutable.
A new attempt needs fresh owner approval plus independent confirmation that
the previous attempt was never delivered, with a maximum retry budget.

The coding graph executor claims jobs before launch and saves the first
structured acknowledgement while the harness is still running. It preserves
that evidence through a failure or Stop. Settlement applies only to the
attempt that produced the result.

Before allocating a graph checkout, the same atomic claim records the approved
project UUID, canonical project/Git directories and base commit. After successful
preparation, X records the resolved checkout commit before starting a provider or
test process. A failure to persist that record prevents launch and retains the
checkout. An interruption between allocation and the second record therefore
leaves an inspectable reservation rather than an undiscoverable directory.

`hex agent workflow worktrees [run-uuid]` is a read-only recovery inventory.
It revalidates the registered project identity, generated X path, Git worktree
registration and base ancestry, then reports the observed HEAD and whether the
checkout is free of tracked, untracked and ignored files for possible cleanup.
The recorded outcome is shown separately: inspecting files never turns an
interrupted claim into success. Missing directories, unsafe or foreign paths,
and older claims without a recorded reservation remain distinct. The default
view inspects at most 64 recent preparations and labels older entries as not
inspected; supplying an exact run UUID inspects that run's entries.

This command does not recreate, retry or delete anything, restore grants, or
show prompts/provider identifiers. Cleanup still uses the separate approved
manager operation, which rechecks ownership and refuses dirty or ignored files;
the inventory's clean flag is not approval. Regression coverage includes a
failed second allocation after an edited first checkout, restart before the
prepared-commit record, retained merge conflicts, symlink/foreign paths, ignored
build output, changed registered project identity, and unchanged journal bytes
after inventory reads. These are local fixture checks, not physical desktop QA.

### Proof-based verification (development)

`x_agent::verification` computes `Verified`, `Failed` or `Unverified`
from direct evidence and required checks. A passing model summary or a
screenshot interpretation cannot satisfy a required check. A harness run can
prove **delivery** only when the correct structured session acknowledgment
and exit code 0 are both observed; it does not prove code correctness.
A separate Git status check reports whether an exact project-relative file
changed, not whether the resulting code works.

Coding graphs without a required proof profile use delivery-only evidence. The task and saved report
label those nodes Delivered, not code-verified. Older saved coding graph
summaries are read as delivery-only. Existing v2 requires edges retain their
historical successful-delivery semantics; they do not establish that a
predecessor produced a usable artifact or passed tests. Version 3 graphs may
add `requires_verified` edges: delivery-only predecessors cannot release
those jobs. Version 2 inputs retain their existing meaning. An OpenCode node may declare `required_change` with a project-relative file
path. X previews the requirement and independently compares its owned checkout
against the pinned base, including committed and untracked changes. Missing
changes fail the node; unreadable evidence is Unverified. This is file-change
proof, not proof of correctness, tests or handoff.

A v3 OpenCode node can instead set `required_commit: true`. X verifies
that the owned checkout ended at a new descendant commit, with a changed
Git tree and no tracked or non-ignored uncommitted changes. It records the
exact base and output commit IDs, project, run and node in the durable claim
journal. `hex agent workflow claims` exposes that artifact evidence locally
without revealing provider session IDs or prompts. The receipt binds this
proof profile. The approval preview also lists two exact Git commands that
this output profile permits: stage changes inside the owned checkout and
create the node's fixed-message commit with hooks and signing disabled. X
supplies those commands in the provider request; it does not commit the
provider's edits silently, grant wildcard Git access, push a branch, or
advance the primary checkout. Ignored build outputs are not part of the
artifact.

Version 4 adds `inputs: [{"node":"producer", "proof":"committed"}]` and
`proof: "tested"`. These refer to reviewed nodes in the same project and run,
never caller-supplied SHAs or paths. Inputs are dependency edges: a delivery
receipt or file-change observation cannot substitute for a committed artifact;
a tested edge requires an actual successful test job. Cycles, missing or
cross-project producers, incompatible proof types, unknown fields, and more
than eight inputs are refused before approval.

A ready job gets its worktree lazily. X independently rechecks each source's
owned checkout, exact commit and clean state. One input starts at that exact
commit. Multiple inputs are assembled in declared order in a new isolated
checkout; a conflict stops that node and retains the conflict for inspection.
The primary checkout, including dirty and untracked owner files, is preserved.
Skipped or queued-cancelled jobs do not allocate worktrees. Recovery claims
begin before preparation so a failed preparation is not silently replayed.

A v4 test job is `{"type":"test", "project_id":"<registered UUID>",
"suite":"cargo_workspace"}` and needs at least one input. Other fixed suites
are `cargo_agent`, `npm`, `pnpm`, `pytest`, and `go`. The normal approval preview
identifies the exact input recipe and suite and discloses that repository
scripts execute with the owner's OS access. Just-in-time one-use permission
binds the resolved checkout and SHA. The existing bounded test runner runs
that suite, verifies unchanged tracked/nonignored state and HEAD, and records
suite, commit, observed time and pass/fail/unknown evidence. Its success is
**Tested for that suite**, not general code correctness. Failure, cancellation,
timeout or changed code blocks tested descendants. `workflow claims` exposes
this evidence and the tested artifact. Restart does not restore any permission.

Only a workspace-capable provider can consume an artifact checkout. The
current tool-free Claude adapter remains available for goal/explicit-context
analysis; it is not offered a checkout that it cannot inspect.

Regression coverage uses actual Git branches/worktrees and owned fixture
provider processes: two independent changes are merged, a real Cargo suite
passes or fails against the combined commit, and the downstream process sees
that exact tested tree only after a pass. Negative cases cover foreign/dirty
artifacts, conflicts, stale scopes, cancellation, missing proof and schema
forgery. These checks do not establish physical desktop or live commercial
provider acceptance.

### Verified worktree scheduling (development)

The DAG scheduler now supports worktree-scoped resource locks. Jobs in the
same registered project may overlap **only when a trusted executor returns
two separately verified worktree identities or native exact allocation reservations**. Without this proof they keep
the existing one-writer-per-project restriction. Mixed project-wide and
worktree-scoped jobs still conflict, and the exact lock is preserved through
the job's lifetime even if the executor's discovery state changes.

X allocates and verifies these identities through Git after approval.

### Approved two-harness execution (development)

The new `workflows::harness_exec` adapter can route a typed DAG node to
either an approved OpenCode or Claude Code adapter in a separately owned Git
worktree. It rechecks project/worktree identity, uses the crash-safe job
journal to refuse duplicate sends, and derives completion from structured
harness delivery and the explicitly chosen verification profile.

Use a v2/v3 coding graph for existing harness jobs or a v4 graph for artifact
inputs and test jobs. Each job references a registered Git project ID.

```sh
hex agent workflow import-graph ./coding-graph.json
hex agent workflow run-graph coding-graph
hex agent approve <task-id>
hex agent stop
```

The plan shows the exact goal, project, provider, and pinned commit for
every job. The same task appears in the Agent pane for approval. Approval
issues exact queued-job grants; ready jobs create their separate folders
under `~/X Worktrees` just before execution.
A dependency can dispatch only while its grant is current and its required
predecessors have verified delivery.

Tests use isolated real Git worktrees and fixture provider executables to
check approval, replay refusal, changed configuration, and dirty-checkout
preservation. Code correctness requires separate verification.

### Reviewed mixed desktop and coding graphs

Imported graphs may combine `desktop`, `harness` and `test` nodes in the same
DAG and scheduler. `hex agent workflow run-graph <id>` produces one explicit
review with separate desktop permissions and coding/test scopes. The desktop
adapter reuses the existing sequential workflow `Host`, preflight checks and
step implementations. It does not create a second action registry or turn a
model response into executable instructions.

Desktop nodes support the existing typed app, directory, terminal, URL,
window-focus/move, workspace, window-wait and run-opened-window-close steps.
Low-risk `RunAction` phrases remain registry-validated and are pinned to their
exact parsed actions. Interactive Computer, typing, coding-session and contextual
window actions cannot enter through that generic phrase path: they need their
own scoped service plan or an explicit typed window/harness node. A graph's
legacy `OpencodeTask` desktop step is refused; coding uses the owned harness node.

Existing window targets bind their exact ID, process and class during review.
A later focus/move may instead bind the window actually observed by one
unambiguous ancestor opener; graph dependencies, rather than array order,
establish that relationship. The native review identifies that source. Closed,
replaced or ambiguous targets fail without selecting another window.
`CloseOpenedWindow` requires an explicit predecessor opener and retains the
existing restriction to windows newly opened by this run; reusing an existing
window does not grant permission to close it.

Every mutation takes the shared service desktop lease, then rechecks the exact
target and Stop token. Focus, workspace changes, moves and closes require actual
readback before returning verification. The lease is released during window
polling and is never held by Git preparation, harness computation or tests.
The same lease protects foreground Actions and other native mutation paths.
Mixed graphs remain foreground-only; only coding/test graphs may detach into
background Work.

Successful browser dispatch is `Launched`, not verified page loading. Ordinary
`requires` edges may accept that dispatch; `requires_verified` requires observed
proof. Per-node failures with `on_failure: continue` block their required
descendants while independent siblings continue. An explicit stop-on-failure
policy and global Stop retain their existing behavior. Individual Stop reaches
only the selected node and its dependent consequences.

Desktop nodes use the same durable claim journal, with their native outcome
separate from provider delivery. Interrupted effects remain unresolved and
cannot be replayed using provider non-delivery approval. Recovery inventory
labels desktop nodes as having no coding checkout. Saved results contain typed
per-node outcomes; native step evidence distinguishes actual window readback
from a launch with unknown effect.

Local integration tests combine the existing fake desktop host with real owned
provider subprocesses and Git commits: app → coding artifact → exact-window
focus/close, a held global input lease while independent Work runs, targeted
Stop, stale PID refusal, and launch-only proof blocking. These checks do not
establish physical desktop or commercial-provider acceptance. Authoring a mixed
graph currently uses import; the native Work composer builds coding/test graphs.

### Harness ownership and output evidence

Dropping an owned coding run requests Stop and waits for its supervisor;
failure to create that supervisor also cleans up the child. The process
adapter bounds individual output frames and its reader queue, rejects
unreadable or oversized frames, and retains only a short stderr tail.
Once a structured session is acknowledged, later events cannot switch the
run to a different session. A process that closes its output but needs
forced termination is reported as failed even if its termination handler
exits with code zero.

On Windows, an owned provider starts with its primary thread suspended. X
assigns it to a private, unnamed Job with kill-on-close and no breakaway before
resuming the one freshly validated primary thread. Assignment, thread identity
or resume failure refuses startup and cleans up the retained process handle.
Stop uses `TerminateJobObject`, without an external `taskkill` helper or a PID
lookup. Linux retains the direct child unreaped while checking the owned
process group and bounding output drain after the root exits. A nonempty group
is terminated before the worker releases its lease; failed observation keeps
cleanup pending instead of claiming success. This Linux procfs observation is
not a kernel Job guarantee, and cannot cover processes that leave the group
or external services. Other Unix systems retain their existing termination
path. Linux fixtures check a surviving descendant and independent group Stop;
actual desktop/provider acceptance remains separate.

Windows closure requires both the direct process to exit and
`QueryInformationJobObject` to report zero active processes. X closes the
exited direct process handle before waiting for Job accounting to drain.
The five-second closure threshold publishes a bounded
`termination_unconfirmed` event; it does **not** finish the worker, release its
project lease, or permit another writer. X keeps requesting native termination
and observing the same Job until closure is confirmed. Dropping the run or a
supervisor startup failure uses the same ownership guard. An exited provider
whose remaining Job members require termination cannot report successful
completion, even when the provider exited zero.

The Windows supervisor observes the direct process exit independently of pipe
EOF. A descendant can retain an inherited output handle after that exit,
including when its own standard streams were redirected. X drains remaining
events for at most five seconds after the direct process exits, preserving an
acknowledged session, then records failure and enters the same owned Job
cleanup. Continuous descendant output cannot extend that drain deadline. This
adds no total deadline to a provider whose direct process is still running.

`harness::RunResult` adds optional typed `termination` evidence with
`scope: windows_job_empty`, elapsed milliseconds and whether forced termination
was requested. Its absence grants no native closure proof. Work summaries and
stopped/failed graph job details retain the confirmed closure message. This
evidence concerns the owned Job, including ordinary inherited descendants;
it does not establish rollback, code correctness or termination of work
delegated to an external service. Windows Jobs are not an application sandbox.
The API contracts are documented by Microsoft for
[Job inheritance](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects),
[suspended creation](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags),
[resume counts](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-resumethread)
and [active-process accounting](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_accounting_information).

Independent Git status evidence uses a deadline and a bounded, complete
capture. Inherited checkout overrides, malformed porcelain records,
duplicate paths and truncated helper output cannot become proof of a
successful coding result. Status evidence still does not establish file
contents or code correctness.

Checks live in `harness::process::tests`, `harness::evidence::tests`,
`opencode::tests` and `desktop::capture_limit_tests`. The disposable X11
smoke test waits for its own test window to become readable before checking
window operations; all later reads and observed effects remain required.

`harness::process::closure::tests` checks that timeout and unavailable readback
retain an execution lease until an independent empty result arrives.
`harness::process::windows::tests` re-executes the actual Rust test executable
to spawn an immediate child and grandchild (including a nested child Job), keeps an unrelated process alive,
checks Stop before resume and Drop cleanup, and rejects a zero-exit parent
that leaves a live child. These native tests run under the existing Windows
Compile job's `cargo test --locked -p x-agent --all-targets`; a Linux target
check alone is not execution evidence for them. They exercise process
ownership rather than installed desktop or commercial-provider acceptance.

### Coding workflow result history (development)

After an approved graph run, X saves an outcome-only summary in its private
workflow directory. Run `hex agent workflow last-graph <id>` to inspect the
last per-node launched, delivered, verified, tested, failed, cancelled or
unverified status. Private
prompts, command output, provider messages and request fingerprints are
excluded. Only the recorded proof profile is established; general code
correctness still requires appropriate verification.

### Coding workflows in Rune (development)

The Agent pane's Workflows view lists imported v2/v3/v4 coding graphs separately
from sequential desktop workflows. Their names and job counts appear without
private coding goals. "Review & run" opens the existing explicit approval
preview; it does not start jobs directly. Mixed desktop/harness graphs use
the same review and supported-node preflight. Saving a legacy workflow under
an existing graph ID is refused rather than overwriting the graph.

### Interrupted coding jobs (development)

Run `hex agent workflow uncertain` to inspect workflow jobs that the
durable journal still marks as claimed after a crash, disconnect or restart.
The read-only result includes the run ID, node ID and reconciliation status;
it does not expose coding prompts, request fingerprints or provider evidence.
X never retries or marks these jobs successful without independently
checking delivery and obtaining fresh approval.

The Rune coding-workflow cards also show the saved count of verified
delivery outcomes from the previous run (or a stopped state). This is a
read-only status, not proof of code correctness.

The Rune workflow manager also shows interrupted coding claims, if any.
It displays only the node/run identifiers and the need for manual
reconciliation. Reading this panel never replays a harness request.

### Safe merge-back previews (development)

The Git integration module checks an owned task worktree and computes a
possible merge result using `git merge-tree --write-tree`. It reports
potential changed files and conflicts without changing either checkout, Git
refs or the user's original working tree. Uncommitted task changes block the
preview rather than being silently omitted. A conflict never applies a merge.

Only a separate, freshly approved R4 operation may later integrate, push or
merge the result; **no such automatic operation is enabled here**. Git may
write unreachable preview tree objects to its object database, but does not
update HEAD, the index or worktree files.

### Scoped approval receipts (development)

X has a process-local, deny-by-default grant store for exact project metadata
reads, individually approved harness tasks, and individually approved test
runs. Task grants are tied to a project ID, run ID and node ID and can be
consumed **once only**. A reviewed coding graph grants each exact node up to
30 minutes to wait for its dependencies after approval. A queued receipt cannot
launch a process: the executor converts it to a launch receipt immediately
before dispatch, valid for at most 60 seconds and never beyond the original
queue deadline. Harness grants bind the exact typed job request and required
verification profile, so an edited goal or weaker proof cannot reuse consent.
Project metadata grants are time- and use-limited. Revocation or restarting X
invalidates outstanding receipts. A grant cannot authorize deletion, merging,
secret access, screen capture or external transfers.

The existing approval action issues receipts for the exact coding graph
shown in the preview. Autonomous mode still requires this approval. Receipt
issuance happens inside the service after the owner approves.

### Approved tests in owned worktrees (development)

X's test verifier accepts only typed presets for Cargo, npm, pnpm, pytest and
Go. It revalidates the worktree's ownership and now requires a live, one-use
`RunTests` receipt matching the exact project, run, task, worktree and test preset
before launching a process. A boolean flag alone cannot authorize tests. It
executes without a shell, enforces a 120-second deadline and propagates Stop
to the owned subprocess. One-off project tests enter the native Work approval
path; v4 graph test jobs reuse the same runner on their resolved artifact.
Exit success is **Verified for that test suite only**; failure is **Failed**;
missing, interrupted or timed-out
evidence is **Unverified**. No process output or secrets are retained in the
evidence record.

Project test scripts can run arbitrary code despite fixed arguments, so this
is a privileged R2 operation. Every one-off test or graph test requires an
explicit native preview and approval, including in Autonomous mode. Worktree
isolation is not an OS or network sandbox.

### Desktop capability checks

Run `hex agent doctor --json` to see `desktop.capabilities`, an operation-by-operation
report for the CLI session. Use `hex agent doctor --running --json` to probe
the running service's environment through the local API. Both forms identify
the build, OS, architecture and probe source. Plain-text output shows the same
capability states. `hex agent desktop` retains the desktop-only service report.

The harness section probes the installed OpenCode and Claude Code binaries for
a version, the required CLI flags and reported login state. A configured login
means the vendor reported local configuration. Provider access is checked when
the user runs a task. A nonresponsive or unrecognized status stays unknown.
X does not open either harness's credential files.

Doctor reports ready when the window list probe succeeds and the required
helpers are present. An unsafe key file also blocks readiness. On Windows the
report labels key file permissions as unchecked. The older `supported` list
continues to describe the backend's advertised operations.

- **Supported** means a read-only window list probe succeeded. Hyprland and
  Windows and opt-in KDE Plasma 6 provide this probe.
- **Partial** means the selected backend advertises the operation and the
  diagnostic could not verify it. This includes mutations such as focus, move
  and close, and listing when the backend has no bounded probe or its read failed.
- **Unsupported** means no backend is available in this session or the selected
  backend does not advertise the operation.
- **Needs permission** means KDE Plasma 6's experimental backend has its helper
  and session bus, `KDE_SESSION_VERSION=6`, and needs the explicit `X_ENABLE_EXPERIMENTAL_KDE=1`
  opt-in.

The probe performs no window changes and returns no window names or IDs.
The separate `desktop.editor_typing` field describes literal workflow text.
Hyprland reports `verify_target_on_use` when its AT-SPI registry answers, or
`accessibility_unavailable` otherwise. KDE Wayland, Windows and macOS report
`unsupported` for this operation. An answering registry alone cannot prove a
specific editor or field is writable.

The [desktop contract tests](../../crates/x-agent/src/wm/tests/contract.rs)
exercise the controller with each adapter's advertised operations and check
the report against successful, failed and unavailable reads. The optional
read-only live test checks the selected backend and rejects a stale window
identity. The Sway and X11 isolated tests also check their actual reports.

### OpenCode session discovery

Choose an existing project with `hex agent workspace <path>`, then use
`hex agent sessions list` or `hex agent sessions resume <session-id>`.
Resume selects the existing session in X after a fresh check with the
registered loopback service. The selection is available in `hex agent status
--json` until X restarts or the project changes. Session discovery uses the
service's authorization and shows a refusal if the registration is stale.
It exposes open root sessions whose reported directory resolves to the chosen
project. Use `hex agent sessions send <goal>` to preview Work for that
session, then approve the displayed task. X rechecks the target before send
and uses the supervised private OpenCode runner with its normal command
permissions. A structured event from the same session supplies the delivery
acknowledgement. Stop cancels the owned runner.

After displaying the list, say `Friday, give this to session one` to preview
sharing selected text. Numbers refer to that list for 60 seconds. A displayed
session title also works when it identifies one entry. An exact ID from a
selected or displayed session takes precedence. Refresh the list after
changing projects. X checks the target and the source window before sending.
This path requires approval and uses the deny-all tool policy available
in OpenCode 2.2.7 and later OpenCode 2 builds.

### Delegate a task by voice

Set the project with `hex agent workspace <path>`. Through an enabled X
command input, say `Friday, ask OpenCode to inspect the backend tests`. X
previews the goal and project, then waits for approval. An explicitly selected
OpenCode session appears in the preview and is rechecked before delivery.
`Ask Claude Code to …` chooses Claude Code directly. Doctor shows the local
harness versions and login information. The model configured in the chosen
harness handles the approved run.

### Claude Code Work

The capability report distinguishes workspace-writing OpenCode from tool-free
Claude Code analysis. Claude Code Work explicitly previews prompt-only access;
it cannot satisfy a required file-change proof. No tool flags are relaxed.

With Claude Code 2 installed, choose a project, run `hex agent claude <goal>`,
and approve the task. X uses Claude Code's own login and streams its structured
events. This mode sends the goal with tool access and MCP disabled. To continue
the last successful session X started in that project, run
`hex agent claude --resume <goal>`. Changing project or restarting X clears
the resume target. Stop cancels the owned Claude Code process.

### Command decisions (Perplexity Decider)

X routes looser spoken Actions through OpenRouter's free **Perplexity Decider
V1.3** models (see "Free Perplexity Decider provider" below). They are NOT text
generators: each selects exactly one existing, validated candidate action, or
says the utterance was dictation/unsupported. Deterministic rules run locally
first, without an API call. Work-mode OpenCode tasks still require whatever
coding model OpenCode uses. A saved legacy `jev` planner setting now means
`decider-flash`.

```sh
hex labs on x-agent
hex agent planner decider-flash
hex agent voice auto
hex agent say --mode actions "Could you bring Firefox to the front?"
```

Provide an OpenRouter key through `OPENROUTER_API_KEY` in the **running
app/service environment**, or put it in the owner-only `openrouter-key` file
inside X's application data folder (`app_paths::support_dir`). An export in a
terminal may not reach an already running systemd service. Do not commit the
key. `hex agent planner off` disables remote routing again.

The first decision (simple action routing) sends only the spoken transcript.
When you explicitly grant **Computer** access for an app and opt into remote
screen analysis, the multi-step agent additionally sends the current
accessibility tree's labels/roles, redacted visible text, window title and
recent action outcomes to the fixed OpenRouter HTTPS endpoint. Screenshots are
a separate opt-in (below). Clipboard contents, passwords, selected text and files
are never sent. Long tokens, email addresses and long numeric strings are
redacted before the remote request. Free models are rate limited and may change.

### Decider observe → decide → act → verify

The Decider planner has typed computer decisions, so it **does not**
require a conversational LLM or a generated JSON action script. For a
natural multi-step browser request, X opens/locates the permitted browser
and repeatedly:

1. Reads the current accessible controls and page text.
2. Asks the Decider which offered operation and *current element* to use.
3. Re-checks the decision's probability, target and permission, then acts.
4. Re-reads the actual interface, checks what changed and repeats.
5. Stops on supported visible evidence, user Stop, an unsafe action, repeated
   failure, 15 steps or a three-minute deadline.

When typing, the Decider selects word boundaries in **your own goal**. X copies
the exact substring, never text generated by the model. It can choose a control
by its current snapshot ID, a public website explicitly named in the goal,
safe browser keys, scrolling or a visible completion evidence label.
The executor refuses destructive, security-sensitive, sending, buying,
deleting, publishing and approval controls, and never runs shell commands.
`done` requires an exact visible evidence field; the claim is rejected if it was
already present before the task's actions. Accepted evidence is reported as
`observed`, with an explicit limitation that the requested goal was not
independently verified. A newly visible toast or unrelated label cannot make
the service report a confirmed Computer effect. Goal-specific independent
verification remains required before a future Computer executor can report
confirmed completion.

Control discovery searches the complete bounded accessibility observation,
ranks permitted controls against the goal and their role, and offers at most
90 controls. The same original snapshot IDs appear in the decision choices and
context; ranking does not change target identity. Password and consequential
controls cannot become actionable through a high match score. The request
reports omitted controls and an empty permitted set so the chooser can return
blocked or request another observation. The accessibility collector's own bound
still applies; this does not provide a full browser DOM or general tree pagination.
Regression coverage includes a relevant control after the first 90 elements,
secret-filled observations, and refusal of an omitted target.

When the permitted next target is ambiguous or a user detail is missing, the
Decider can choose `clarify`. X supplies a fixed question locally. A generative
planner's question is bounded and redacted before display. The task enters
`waiting_for_input`; answering requires its exact task ID and opaque question
token. The answer is limited to 300 plain-text characters and creates an
`awaiting_approval` review, including in Autonomous mode. It does not grant
another app or remote model access.

Approval consumes an in-memory continuation of that exact Computer step. X
rechecks the selected model and remote consent, binds the original window's
process identity and obtains a fresh observation before its next decision.
Prior mutations and earlier compound-command steps do not replay; later steps
still run after completion. Original evidence, action/error counts, the 15-step
budget and the three-minute deadline survive the pause. Time spent answering
and reviewing counts toward that deadline. At most three questions can be
answered in one loop. A pause clears the transient permission to press Enter
after typing because user interaction may have changed focus. Stop, expiry,
model changes, missing/replaced windows and revoked remote consent prevent
resumption. Continuations are not serialized and do not survive restart.

The question picker uses bounded text answers. It does not invent stable tab
identities or reuse an old `eN` control reference as a clarification choice.
Synthetic tests cover complete pause/answer/review/resume flows, stale tokens,
Stop, expiry, model/consent revocation, lease contention, retained budgets and
non-replay; physical desktop acceptance still requires a live environment.

Enable remote screen context **explicitly**, on top of X's normal Agent
Labs switch and planner selection:

```sh
hex labs on x-agent
hex agent planner decider-flash
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
content still needs a separate model. Provider errors and low-confidence
choices fail closed rather than guessing or falling back to paid inference.
Deterministic commands still run without contacting the provider. Built-in window
phrases take precedence over speech corrections based on installed app names
and keywords.
Work-mode OpenCode still needs its own configured coding model.

Source: `crates/x-agent/src/decision.rs`, `crates/x-agent/src/decision_computer.rs`, `crates/x-agent/src/decider.rs`,
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
  Agent pane; navigation never goes through the Decider or the Actions executor.
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
trigger (ASCII letters, numbers, `-`, `_` and single spaces between words;
max 48 characters) and up to 4096 characters of
text. Search `snip <trigger>`, `snippet <trigger>`, or `snippets` and press Copy
on an entry. Removing a trigger deletes its stored text.

- Stored under the user's config directory, `hex/x-snippets.json`, as a local
  JSON file. Updates use temporary files and refuse symlinked storage.
- The library is bounded to 100 items. Malformed data is reported; it is never
  automatically overwritten during a Save.
- Explicit `snip`/`snippet` searches match both the trigger and the saved
  body; ordinary Quick input never searches the private snippet library.
- Multiword triggers such as `my email` work with explicit spoken
  `insert snippet my email` and Quick copy. Automatic expansion while typing
  in arbitrary applications is still not implemented.
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

## Offline snippets CLI

The command line can now manage the **same** local X Quick snippet store,
without the app running or an AI model:

```sh
hex snippets list
hex snippets list --json
hex snippets set signature "Best regards"
printf 'A multiline snippet\n' | hex snippets set signature --stdin
hex snippets remove signature --yes
```

The existing 100-snippet and 4096-byte limits, duplicate checking,
owner-only atomic writes and symlink refusal apply to CLI and UI equally.
`--stdin` is preferable for private text because positional text appears
in the system's process argument list. Removal requires `--yes`.
The CLI intentionally does **not** bypass the system-wide keyboard expansion
gap: snippets still need explicit selection or spoken insertion.

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
* **Action previews** carry the service-assigned task and step key, input source,
  risk and required capabilities. A standalone window close previews the resolved
  window. X rechecks its identity before dispatch and asks for a numbered choice
  when several candidates match. Mock window tests exercise focus changes,
  reused native IDs and a candidate disappearing from an ambiguous list.
* **Launched is not verified.** X reports "launched"; it does not claim a page loaded.
* **Work never bypasses OpenCode's permissions.** X does not pass `--auto`. The run uses a
  private OpenCode server (`--standalone`) when the installed OpenCode has it. A run only
  counts as completed with exit code 0 **and** no error event; an error event ends the run.
  Work and saved workflows share the adapter and its structured event parser.
  Only a typed bash refusal can offer a command for separate review. The harness
  descriptor in Agent status gives the event schema and the adapter's operations.
* **Stop** discards a pending plan, prevents queued steps from starting and
  requests cancellation of workers owned by the task. A late model proposal
  is discarded. Say `Friday, stop the task` through the enabled command path or
  use the Agent pane or CLI. On Unix the OpenCode worker signals its own process
  group and escalates after a five-second grace period. X reports changed files
  when Git evidence is available. These changes stay on disk.
* **Local by default.** Speech uses Hex's local model. Screen text is sent to a local
  planner unless you explicitly allow remote screen access. X sends nothing else itself;
  OpenCode talks to whichever provider you configured.
* **Computer use** requires a per-app grant and runs a bounded observe, decide,
  act, verify loop. Browsers, VS Code and file managers are supported within
  their allowed accessibility controls; browser-only keyboard shortcuts never run in
  editors or file managers. Links and a small set of navigation buttons may be clicked;
  other buttons and consequential controls are refused. Remote models additionally
  require explicit `agent computer remote on`; on-screen context is redacted before
  transmission. Reviewed compound Actions plans can include a Computer subtask
  followed by explicit app/window actions; a failed subtask stops the sequence.
  Implicit focused typing cannot be combined with Computer steps, and coding
  sessions still require separate Work approval.
* **Local audit** records task approval, discard, Stop, and action outcomes in
  `x-action-ledger.ndjson` under X's data directory. The ledger stores task and
  step keys, source, risk, requirements and status. Task text, model output,
  credentials, filenames and window titles stay out of it. Known records from
  the previous approval log are imported once, and that file stays available.
  An audit write failure blocks approval and is shown in the task UI.

## Not done yet

* A native listener shortcut inside Hex (today: bind `hex agent toggle` in your desktop).
* Real Firefox and Chrome profile workflows have not been verified on this build.
* HUD phase indicators for Agent work are not implemented; phase state is visible in the
  Agent pane.
* Permission prompts raised by OpenCode are shown as a stall warning after 120 s of silence,
  not yet as an in-pane approve button.

Code: `crates/x-agent` (logic and tests), `src/x_agent.rs` (Hex glue),
`src/rune_ui/agent.rs` (pane), `hex agent` in `src/control/cli.rs`.


### Free Perplexity Decider provider (OpenRouter)

Select `hex agent planner decider-flash` for Perplexity Decider Flash V1.3
(`perplexity/pplx-decider-flash-v1.3:free`), or `hex agent planner decider`
for full Decider V1.3 (`perplexity/pplx-decider-v1.3:free`).
Both use **OpenRouter's Decisions API**, not chat completions, and make
choice-only decisions over X's same locally validated action candidates.
Existing deterministic commands bypass remote models.

Set `OPENROUTER_API_KEY` in the **running X service environment**, or save
the key in the private `openrouter-key` file in X's application data folder.
Do not commit or print the key. Free endpoints have rate limits; on 401, 429,
or model failure, X refuses the remote step without a paid-model fallback.

```sh
hex agent planner decider-flash
hex agent say --mode actions "Bring Firefox to the front"
hex agent planner decider
hex agent say --mode actions "Bring Firefox to the front"
hex agent planner gemini  # or another planner
```

This integration uses text and the accessibility tree for decision context.
Screenshot support is a separate opt-in capability; it is never collected
automatically by basic command routing.


### On-demand screenshots and private visual decisions (Hyprland)

Manual local screenshot capture requires `grim` and, for the region
selector, `slurp`. Capture output never goes to an API:

```sh
sudo pacman -S --needed grim slurp
hex agent screenshot screen
hex agent screenshot window
hex agent screenshot region
hex agent screenshot window --output "$HOME/Pictures/my-window.png"
```

These capture a single screen (full compositor), the **focused window's
rectangle**, or an explicitly dragged region. Images are written only on
request, defaulting to unique PNG filenames in Pictures with mode 0600;
existing files are never overwritten.

**Decider image context is a separate opt-in:** only after app-grant and
remote-screen permission are already enabled, also run:

```sh
hex agent planner decider-flash  # or: decider
hex agent computer grant firefox
hex agent computer remote on
hex agent computer vision on
hex agent computer list
hex agent say --mode actions "In Firefox, click that button on screen"
hex agent computer vision off  # immediately disables future visual sends
```

The computer agent captures only the presently focused, granted app window,
and only on the first decision of a task where the user refers to visual
elements or the accessibility tree is empty. No capture is made for basic
native Actions, typed dictation, or every computer step. The PNG stays in
memory, is base64-encoded directly in the explicit Decisions API request,
and is discarded when that request completes. X does not persist a remote
vision screenshot in /tmp or retain screenshot history.

**Privacy and limitations:** screenshot pixels can contain private content
that AT-SPI text redaction cannot protect. Vision opt-in is independent of
remote accessibility-text consent and is disabled by default. Only turn it
on when comfortable sharing visible pixels with OpenRouter/Perplexity.
Images are supporting context for **existing allowlisted accessible UI
targets**; X cannot use ungrounded coordinates to click inaccessible
buttons, and screen text never grants new permissions. Model quotas apply.

### Captured-context expiry

Before targeting a previously observed window or applying a selection-based edit,
X rechecks both window identity and snapshot age. Context older than 15 seconds
is rejected even when the same window still exists and even after an approval
prompt temporarily changes focus. The user must issue the command again to
obtain fresh evidence.

### Inspect a coding graph in native Work

Foreground and background graph steps show their actual scheduling stage,
elapsed time, and earlier jobs that must settle before they can proceed.
Provider and fixed-suite jobs show preparation until X has recorded dispatch.
A completed job links to its exact recorded checkout once X has durable
checkout evidence. Stop and renewal continue to use the exact task and step.

### Review an expired queued coding approval (A05)

A ready coding or fixed-suite test node whose exact queued receipt expired enters `waiting_for_permission` without launching or holding a worker slot. The scheduler releases idle project reservations and requires a fresh project lease before dispatch. Independent jobs keep running; required dependents still wait. The operator can inspect the original reviewed step using `hex agent workflow permission-waits <task-id>`, then explicitly approve only that unchanged step with `hex agent workflow renew-permission <task-id> <review-token>`. The token expires after ten minutes; renewal grants up to five minutes of queue time once. The original dispatch revalidates project, immutable base, inputs, provider role and the one-use launch receipt. A Stop, revocation, missing receipt, changed identity, stale token or second renewal cannot recreate authority. The foreground or exact background task remains identifiable by its task ID; no restart restores renewal authority.

The renewal control covers queued coding and test nodes before worker dispatch. If approval expires during checkout preparation, the job fails without launching and the exact checkout remains available for inspection. An expired desktop approval prevents native input, including expiry while waiting for the input lease. The owner can review a fresh plan for either case. The native task card and each background run show the exact waiting step, its original work scope, a single Renew approval button while its review is live, and the existing scoped Stop. The button disappears when the token expires or is revoked. Physical queue/desktop acceptance remains pending. Deterministic grant fixtures and a 61-second subprocess predecessor exercise expiry, one-use renewal and predecessor non-replay. No test or review token is a general process-launch grant.

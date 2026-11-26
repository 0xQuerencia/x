# Native Work composer

The **Work task composer** in Labs → X agent → Work creates a single provider
task or a saved workflow with two coding tasks. It uses the existing project
registry, session picker, workflow validation, owner approval and execution
service. Creating a draft or preview does not issue job grants, start a
provider, run tests or allocate worktrees.

For a local Markdown roadmap with stable task IDs, completion markers and
dependencies, [roadmap proposals](roadmap-import.md) prepare up to three
independent changes through the same graph review. That entry point currently
uses the CLI; the native composer remains the single/two-task form described here.

## Create and review a task

1. Use **Browse projects** and select a registered project. The composer shows
   its saved name and exact directory.
2. Open **Compose tasks**, then choose **Single task** or **Two tasks**.
3. Choose a provider and enter a task name, goal and optional acceptance
   criteria. Provider choices show the adapter's actual role. OpenCode supports
   workspace changes. The restricted Claude Code adapter supports analysis of
   supplied text; it cannot supply code artifacts for the two-task workflow.
4. For a single task, choose **Fresh session** or the session selected in the
   existing picker. The service compares that exact session and registered
   project to current state before preparing the preview. Stale choices fail
   instead of falling back to another project or a new session.
5. Press **Preview Work plan** and review the normal task card above the
   composer. Execution uses the existing approval action and live inspector.

Single-task Work uses the selected project directory and the provider's
reviewed access. Two-task workflows start fresh sessions in separate X-owned
Git worktrees; a session in the original directory cannot be resumed in a
different checkout.

## Two tasks, dependencies and tests

Both coding tasks must produce a changed, clean descendant commit. Their
goals and acceptance criteria appear in the reviewed plan. Choose the data
flow explicitly:

| Relationship | Starting code | Worker limit |
|---|---|---|
| **Independent** | Each task starts from the same reviewed project commit in its own checkout. | One or two at a time. A one-worker plan adds an ordering edge without importing the first task's code. |
| **Task 2 uses task 1** | Task 2 consumes task 1's exact committed output. Missing or failed artifact proof blocks task 2. | One at a time, enforced by the artifact dependency. |

**Review commits** requires artifact evidence without claiming that tests ran.
**Run project tests** adds a typed test node using a fixed Cargo workspace,
Cargo x-agent, npm, pnpm, pytest or Go suite. The form shows the executable and
arguments; approval covers running repository scripts in the owned checkout.
For independent tasks, the test node combines task 1's commit followed by task
2's commit. For dependent tasks, it tests task 2's descendant commit. Conflicts
and failing tests retain their own failure evidence. Changes are integrated
into the original project only through the separate reviewed integration path.

The service saves two-task plans as version 4 graphs through `Store::import_graph`
and invokes the existing graph preview. They remain available in **Workflows**
for later review. Invalid goals, unsupported provider roles, stale project
identities and invalid worker limits do not launch work. If a valid graph was
saved but its preview fails, the composer reports that the saved workflow
remains available.

## Implementation and checks

`src/rune_ui/agent/composer.rs` owns the Rune form and asynchronous preview
feedback. `crates/x-agent/src/service/composer.rs` compiles the typed draft and
enforces selection and capability checks. The graph schema and executor remain
the shared implementations in `workflows` and `service/harness_workflow.rs`.

Service tests cover approval without dispatch or grants, exact artifact/test
recipes, one-worker ordering, rejected invalid drafts, pinned project/session
selection and the distinction between tool-free analysis and workspace writes.
Native compilation, interaction and rendering checks are separate from these
service tests; fixture evidence does not demonstrate an installed provider run.

`src/rune_ui/agent/composer/tests.rs` exercises the real Rune form: keyboard
focus and goal editing, pointer selection of dependencies and test suites, and
provider choices that respect writer versus analysis capabilities. The explicit
`render_native_work_composer_light_and_dark` test renders the production form
through the window compositor to `tests/golden/rune-work-composer-{light,dark}.png`.
Run it with `cargo test --locked --bin hex
rune_ui::agent::composer::tests::render_native_work_composer_light_and_dark --
--ignored --exact` and inspect both images when this form changes. It fails when
no GPU or software Vulkan adapter is available; it does not silently claim a
visual pass. These component renders supplement the full Rune golden suite.

## Correct an observed failed test (B14)

A failing graph test can expose its bounded stdout/stderr tail in a local-only, in-memory failure review. Select the same registered project and inspect the exact failed run/node from workflow claims with `hex agent workflow correction-preview <run-id>/<node-id>`. Then enter an explicit owner correction goal with `hex agent workflow plan-correction <run-id>/<node-id> "Fix the failing assertion"`. The ordinary task review shows the failed commit, OpenCode goal, isolated checkout and the same fixed test suite. Approve the new task separately; merely reading the failure or planning does not launch work.

At dispatch, X independently rechecks the recorded failing test, worktree identity, clean inputs and immutable commit. The correction writer starts at that exact failed commit and must produce a changed descendant commit before a fresh test node can retest it. No automatic integration, remote push or silent retry is authorized. The failure text remains untrusted and local; only the explicit reviewed goal is sent. A live chain permits at most two correction attempts, one approval per source failure. Restart, a stale source, or the 30-minute review retention limit requires a new independently reviewed task, not replay. The in-memory review holds at most 16 results and is not restored from journal history.

Fixed suites use the existing owned process guard. Linux and macOS retain the project lease until the launched process group is independently observed stopped, and Windows uses a private Job. Each output stream retains at most 8,192 input bytes, and the capture loop closes its pipes on Stop, timeout or read error. Process groups cover members that stay in the owned group. The Git and Cargo fixture exercises real descendant commits, passing retests, failed attempts, stale source and local-only diagnostics. The native correction panel and installed-provider acceptance remain pending.

## Explicit mixed Actions and Work proposals (C09)

In Actions mode, say or type `open VS Code then ask OpenCode to fix the UI then run cargo tests`, or use the explicit `plan mixed` prefix. X builds a bounded version-4 graph using the selected registered project. It accepts supported open/focus actions, one fresh OpenCode writer and optionally one fixed test suite. Every clause must be understood; an unsupported clause rejects the proposal. Following steps require verified predecessor outcomes, and tests consume the exact committed coding artifact. The ordinary graph review is mandatory even in Autonomous mode. The proposal itself starts no providers, worktrees or desktop actions.

This is a finite local grammar, not arbitrary mixed-goal generation. It does not authorize application input, model-generated commands or reuse of a selected provider session. Parser/graph tests cover recognition, proof edges and rejection; actual mixed desktop/installed-provider acceptance remains separate.

## Review a recorded worktree result (B03/B11)

Run `hex agent workflow worktrees` to locate a recorded run UUID and node ID, then `hex agent workflow review-worktree <run-id> <node-id>` for the local JSON review. It verifies the registered project and checkout identity, shows the exact base and observed HEAD, a committed patch capped at 24,000 input bytes, and whether a recorded pass or failure matches the clean checkout. The response flags truncation, binary changes, tracked edits and untracked files separately. A dirty checkout marks test evidence stale. The patch contains only committed changes. Review requires a recorded preparation and refuses an active project lease or an uncertain claim.

This explicit local command returns repository text to the operator, not to the Decider or a coding provider; patch contents remain untrusted. It does not authorize tests, correction, integration or cleanup. In native Work, **Review Work results** lists the most recent recorded checkouts. Selecting a result shows the exact run and node, current and recorded commits, test evidence, committed file statuses and a bounded patch. Stale evidence, uncommitted files and truncated output are marked.

**Open checkout** revalidates the exact Git-owned path. **Review fast-forward** is available for a current clean committed result. It opens a separate review of the primary checkout and never applies a merge on selection. The approved merge rechecks the exact reviewed commit and worktree. The Git fixture checks actual commit content and stale-test handling; installed-operator acceptance is still required.

## Check delivery for a recorded job (A02/A09/B11)

Run `hex agent workflow reconcile-coding <run-id> <node-id>` with an exact
identity from **workflow claims**. For an acknowledged OpenCode claim, X
revalidates the registered project and owned checkout, then asks the registered
loopback OpenCode service for the exact recorded session. The response reports
the recorded outcome, acknowledgement, and whether the service returned that
session in that checkout. The command reads session metadata only.

`hex agent workflow open-worktree <run> <node>` opens an independently
validated owned checkout from the inventory, including interrupted
preparations. Opening carries no provider, test, or Git authority.
`worktree-cleanup-preview <run> <node>` displays the exact clean checkout
and review token. `worktree-cleanup <run>/<node> <token>` uses Git's
non-force removal after checking ownership, commit and status again. Cleanup
requires a stopped or failed preparation with durable evidence that dispatch
never began. Retained journal records and the primary checkout remain in place.

To retry an independent coding node, select the matching saved workflow and run
`hex agent workflow plan-coding-retry <workflow-id> <run-id>/<node-id>`.
Review the unchanged request, the recorded base commit and the attempt number,
then approve the exact new task. A retry keeps the recorded run and node.
It requires durable proof the prior attempt never reached dispatch. It can
reattach a clean prepared checkout at the same commit or prepare an absent,
previously unprepared checkout. The limit is three total attempts. Delivered,
ambiguous, dirty, stale and dependency-bound requests remain unavailable for
retry. A retry never replays earlier nodes.

For an interrupted first attempt before durable checkout preparation, X can
report that dispatch was never reached. After preparation a missing session
remains uncertain. Claude Code lookup is unsupported. Every result leaves the
original claim intact and keeps retry authority disabled.

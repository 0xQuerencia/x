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

This CLI slice does not yet include the dedicated native correction panel or independently verified cross-platform test-process descendant cleanup. The unit tests exercise source binding, explicit proof edges and the attempt budget; real installed-provider correction/retest acceptance remains required.

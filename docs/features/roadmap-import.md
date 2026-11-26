# Markdown roadmap proposals

X can inspect an explicitly selected local Markdown file and prepare one to
three declared independent coding changes through the existing Work graph
review. No model is required. Inspection does not save a graph, allocate a
worktree, issue permission grants, launch a provider or run tests. Planning
saves a proposal and opens ordinary owner approval; only approval enters jobs
into the execution queue.

The secure source reader is enabled on Unix builds, using native
no-follow handles for every path component. Other platforms return an explicit
unsupported-reader error. This is a source capability boundary, not a claim
of physical desktop or installed-provider acceptance.

## Select and inspect

Select a registered Git project first. Keep the roadmap inside the configured
home directory, as a regular `.md` or `.markdown` file. Symlinks, linked
ancestor directories, parent traversal, credential directories, non-UTF-8
data, control characters and files larger than 64 KiB are refused.

```sh
hex agent projects use PROJECT_ID
hex agent roadmap inspect /home/me/projects/app/roadmap.md --project PROJECT_ID
```

The result shows the exact source path and SHA-256, registered project, current
base commit, all task dispositions, suggested `next` task IDs, and an
`inspection_id`. The inspection binds file identity and bytes, project identity
and commit, and the actual saved roadmap proposal inventory. Copy the returned
inspection ID for the planning command. A changed source, repository base,
selected project or saved inventory requires a fresh inspection.

## Explicit Markdown format

Use a stable roadmap ID and stable task IDs. The complete example is
[`docs/examples/x-roadmap.md`](../examples/x-roadmap.md).

```md
# X Roadmap: sample-app-v1

- [x] SETUP: Repository scaffolding accepted
- [ ] UI: Keyboard controls
  Goal: Add explicit keyboard shortcuts to the task list
  Acceptance: Keyboard interaction tests pass
  Depends: SETUP
- [ ] API: Worker status
  Goal: Return observed worker state from the status endpoint
  Acceptance: Status tests cover running, failed and stopped jobs
  Depends: none
```

Unchecked tasks require exactly one `Goal`, `Acceptance` and `Depends` field.
`Depends: none` declares independence from other unfinished tasks. Otherwise,
list stable task IDs separated by commas. A task is ready only when all its
declared predecessors are checked complete in the source. Missing IDs,
duplicate IDs or fields, cyclic dependencies, more than 128 entries, and task
instructions longer than 4,096 characters are refused. Task IDs use 1–32 ASCII
letters, digits, underscores or hyphens; titles use 1–80 characters.

Blank lines and bounded `##` section headings are allowed. Other prose, fenced
commands and unknown fields are rejected. A field such as `Approved: true`,
`Provider: ...` or `Command: ...` cannot add authority or executable operations.
Instruction-shaped text inside a goal remains task data shown in the normal
review. Provider selection, fixed test suite and approval come from explicit
user controls.

## Completion and duplicate inventory

The importer separates these states:

| State | Meaning |
|---|---|
| `marked_complete` | The author checked `[x]` or `[X]` in the selected source. This is an explicit existing-work inventory, not independently verified implementation. |
| `saved_proposal` | An actual saved graph contains the same roadmap/task identity or exact goal-and-acceptance fingerprint for this project. The graph ID and any typed last run outcome are shown separately. |
| `blocked` | One or more declared dependencies remain unchecked. A previous delivered or verified artifact does not automatically accept the feature or check off its source task. |
| `ready` | The task is unchecked, has no unfinished declared predecessor, and has no matching saved proposal. This is readiness for human review. |

Only exact identities and normalized task text are compared. Semantic matching
against arbitrary repository code, issues or pull requests is unsupported.
Review that inventory yourself and mark already accepted features complete
before preparing changes. Unchecked text is never presented as proof that a
feature is absent. Likewise, declared independence does not prove that edits
cannot overlap; review dependencies and acceptance criteria against the actual
repository. The selected project commit is shown explicitly.

Saved proposals and completed source tasks remain distinct. A rejected pending
task can be reviewed again using `workflow run-graph GRAPH_ID`. To discard the
saved proposal, use `workflow delete GRAPH_ID`, then inspect again; its tasks
become eligible without rewriting completion markers. The importer never
edits or checks off the Markdown file. Existing unresolved job records retain
their normal reconciliation rules.

## Prepare the selected changes

```sh
hex agent roadmap plan /home/me/projects/app/roadmap.md \
  --project PROJECT_ID --inspection INSPECTION_ID \
  --task UI --task API --task DOCS --suite cargo-agent
```

Choose one to three ready task IDs. Omitting `--task` uses the inspected next
up to three. Edit goals, acceptance criteria or dependencies in the Markdown,
then inspect again to review the changed proposal. `--provider open-code` is
the default; the service checks the adapter's actual owned workspace-writing
capability. A tool-free analysis adapter cannot be selected for these changes.

Each selected task becomes an ordinary version 4 required-commit Harness node,
starting independently at the same pinned base in its own owned worktree.
The existing graph scheduler permits up to two workers at once. Failures block
required dependents while independent permitted tasks continue.

The optional `--suite` selects an existing fixed Cargo workspace, Cargo x-agent,
npm, pnpm, pytest or Go suite. The test node consumes all selected committed
outputs in displayed order, using the existing integration worktree and exact
commit test runner. The ordinary preview shows the exact executable and
arguments. Conflicts and test failures remain failures; model text cannot
supply test proof. Approval still covers running repository scripts with the
user's OS access. No change is integrated into the primary checkout by import.

The saved graph carries bounded, non-authoritative source provenance in the
same atomic owner-local file. It adds no triggers, grants or execution registry.
Generic JSON graph import refuses supplied roadmap provenance; create it from
the explicitly selected Markdown inspection instead. A copied annotation cannot
select an additional source file through that import path.
Saving a duplicate source task is serialized and refused. Source bytes and
project/base identity are checked again when a saved graph is reviewed and
immediately before its first dispatch. A new source state requires a new
proposal; it never silently replaces the approved instructions.

## Check linked implementation evidence

An optional `Evidence` field accepts up to three distinct links:

```md
  Evidence: https://github.com/owner/repo/pull/123 @ 0123456789abcdef0123456789abcdef01234567
```

Use the full merge commit. Inspection labels each task's mapping `unmapped`,
`needs_review`, or `ambiguous`. Duplicate identical links collapse into one.
To check an exact link, use `hex agent roadmap evidence FILE` with the project,
current inspection ID, task, repository, account, PR number and merge commit
shown by `--help`. The registered checkout needs a matching GitHub remote URL.
X reads the selected PR through the installed GitHub CLI, checks its merge
commit and verifies ancestry in the inspected local checkout. A changed
source, wrong project, stale merge or absent local ancestry prevents a
confirmed mapping. Source completion and run approval stay under owner control.

## Implementation and verification limits

`workflows/roadmap.rs` parses the structured document and validates provenance;
`workflows/roadmap/source.rs` owns bounded source reads. The existing `Store`
provides saved graph inventory and atomic duplicate refusal.
`service/roadmap.rs` selects tasks and invokes the normal pinned graph review.
The `agent roadmap` CLI uses the existing workflow control transport.

Automated tests use real Markdown files, a real Git repository and real graph
storage. They cover inert inspection, three-task artifact/test recipes,
completion and pending-proposal distinctions, duplicate refusal, malformed
and injected fields, source replacement, changed commits/projects, symlinks,
size limits, corrupt inventory and refusal before dispatch when source changes.
They establish these local contracts, not semantic source/PR review, a native
roadmap editing form, physical platform acceptance or a commercial-provider run.

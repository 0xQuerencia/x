# Foreground commands and background work

X uses one service for its foreground task and up to four explicitly approved
background coding or fixed-suite test runs. The native **Approve in background**
button and `hex agent approve <task-id> --background` consume the exact pending
preview. They do not reconstruct a task from its label. A refused approval
preserves the pending task so the user can inspect or discard it.

The foreground is available for local commands while those runs continue. Each
run retains its own task ID, steps, events, provider session, grants, Stop token
and result. An execution lease prevents separate runs from concurrently owning
the same or nested project directories. A coding graph can still use its own
bounded scheduler and isolated worktrees inside that lease. Finishing or
requesting Stop does not release the project until its owned worker has returned.

## Observe and stop

The native command, Quick and workflow views display background runs using the
same service snapshot. History is bounded to sixteen runs and twenty events per
run in the snapshot. A completed run's summary and actual step states remain
visible. Delivery and launched effects use an arrow; confirmed or tested effects
use a check. A provider acknowledgement alone does not mean its code was tested.

| Control | Scope |
|---|---|
| **Stop** on the foreground task; `hex agent stop --foreground` | Current foreground task only. |
| **Stop run**; `hex agent stop --run <task-id>` | One exact background run. Unknown IDs refuse. |
| **Stop job**; `hex agent stop-job <task-id> <one-based-job-number>` | One graph job and its owned process; scheduler dependency rules apply to descendants. |
| **Stop all tasks**; `hex agent stop`; spoken `stop all` | Foreground plus every currently owned background run. |

Discarding a foreground preview or recording does not cancel unrelated coding
work. App shutdown requests all owned work to stop and waits for the execution
leases to be released. An unwinding owned worker revokes its grants, reports an
explicit failure and releases its lease; it cannot alter a newer task.

## Private notifications and activity retention

The native watcher consumes a bounded, content-free completion queue shared by
foreground and background runs. A completion notice is retained until the watcher
consumes its notice, even if another foreground task is already being planned.
Each task emits once; at most 64 pending notices and recent deduplication IDs are
kept in memory. Popups contain only the local task number and a conservative
outcome: confirmed, launched, observed, mixed evidence, delivered, finished,
failed or stopped. They never contain prompts, paths, answers, provider output,
raw errors or selected text. A delivered provider response does not claim tested
code. Full evidence stays in the task/activity view.

On Linux, `hex agent notifications on|off|status` controls the saved preference.
The desktop requires `notify-send` and a notification daemon. Windows and macOS
currently report that this adapter is unavailable. Notices are drained while
disabled, so enabling them does not replay old results. An attention notice opens
the existing Agent review window; only the exact reviewed UI or CLI operation
can grant approval. Dismissing a popup cannot discard a different foreground
plan or stop background work.

`hex agent clear-activity` removes finished in-memory foreground/background
history. Running workers and pending previews remain intact, as do durable
recovery journals and artifacts. Background history otherwise remains bounded
to sixteen runs; restart does not retain this in-memory activity.

## Answer a computer question

When the computer loop needs a detail, the original task enters
`waiting_for_input`. The native task card shows the locally authored question,
an answer field and its remaining time. **Review answer** moves it to mandatory
review. **Approve and resume** continues the same step; prior actions are not
replayed. The CLI shows equivalent commands in `hex agent status`:

```sh
hex agent answer <task-id> --question <question-token> "the exact answer"
hex agent approve-answer <task-id> --question <question-token>
```

Both operations require the exact task ID and current question token. A stale
initial approval, an answer to an earlier question, automatic policy, or a plain
`approve <task-id>` cannot resume the question. Answers are bounded to 300 plain
text characters and there are at most three questions per computer action.

The continuation retains its original app/window identity, evidence, action
counts and monotonic deadline. The default 180-second computer budget includes
time spent answering and reviewing. Resume rechecks the current model, consent,
exact window and fresh observation. It clears any previous typing-to-Enter
permission. Stop, expiry, a changed model or revoked consent cannot be used to
resume the old continuation. The continuation is memory-only; restart does not
restore authority or replay it.

## Wait for actual UI changes

After a Computer mutation, an explicit Wait, or navigation, X polls redacted
accessibility context for a change within 900, 750, or 2500 milliseconds. Every
poll rebinds the original exact window and checks Stop and the original task
deadline. The changed observation directly grounds the next decision. Changing
only the observation token does not count as progress. An unchanged timeout is
reported explicitly and supplies the freshest context; an observation change
does not prove that the requested goal completed. The 300 ms OS key debounce is
retained. Physical app loading and accessibility behavior remain separate
acceptance checks.

## Recovery and verification

`hex agent workflow worktrees [run-uuid]` provides read-only recovery inventory.
It distinguishes recorded job outcomes from current ownership, Git registration,
checkout HEAD and cleanup cleanliness. Missing, foreign, legacy and uninspected
entries are explicit. It does not restart jobs, restore grants or delete files.

Core tests exercise independent real fixture provider processes, cancellation
and process reaping, project contention, capacity limits, panic recovery, content-free notices that survive foreground replacement,
finished-history clearing without stopping active work, exact question tokens, original deadlines and continuation without replay. Artifact
pipeline tests use real Git worktrees and Cargo test commands. These automated
checks do not establish commercial-provider behavior, physical desktop or
microphone acceptance, or release readiness. Native rendering and keyboard
checks belong to the separate UI validation gate.

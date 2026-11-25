# Task-scoped window references

This is the native window slice of the follow-up dialogue contract. It reuses
`Context`'s `WindowRef`, source and confidence types. It does not retain screen
history, selected text, browser tabs or identities from background work.

## Entry points and behavior

Standalone native focus, move, maximize, minimize, close and previous-window
actions bind a concrete window identity before review. The native controller
reads the ID, PID and application class and preserves the requested operation.
It refuses an exact reference when the backend cannot supply a stable identity.
Existing native capability checks and effect readback still apply. Compound
action sequences keep their existing execution behavior; this slice does not
pretend to bind a window that an earlier step has yet to launch.

An ambiguous standalone operation creates `waiting_for_input` and the optional
`window_question` field in Service status. The question contains the task ID,
step, opaque question ID, unchanged operation, numbered window choices with
window-manager provenance, selected/previous ordinals and remaining lifetime.
There are 2–20 distinct choices and the list expires after 30 seconds. X never
silently truncates or reorders a displayed list.

`Service::answer_window(task_id, question_id, answer)` accepts a bounded
deterministic selector: an exact number or ordinal, `this`/`that` for the
already selected choice, `previous` for the previously selected choice, or
`the other one` when the list has exactly two choices and one is selected.
`actually the second one instead` is a supported correction. Arbitrary goals,
new operations, missing referents and ambiguous alternatives are refused.

Answering reads the selected window again, then checks the exact task and
question token a second time. It changes no input or permissions. It updates
the exact target preview, rotates the question token, and requires review.
Corrections retain the original list and deadline. A changed title or list
order cannot change the meaning of an ordinal; a recycled PID or missing
window is refused.

`Service::approve_window_answer(task_id, question_id)` consumes the current
reviewed token under the same lock that starts the foreground task. Generic
approval refuses a pending window question; Autonomous mode cannot bypass it.
The shared desktop input lease is acquired before execution, and the original
reference expiry is checked again after waiting for that lease. The typed
native adapter rechecks exact identity, deadline and Stop after reading native
state and before sending input. No delayed initial approval can approve a
later selection.

Mixed desktop graph nodes and saved workflows pass their own Stop token through
the same exact native focus, move, close and workspace operations. Cancellation
during the adapter's additional identity read is checked before dispatch. A
close request whose window remains open is returned as unverified.

`Service::cancel_window_question(task_id, question_id)` cancels only that
pending foreground window task. A stale token cannot cancel a replacement
task or independent background work. Stop also discards the reference table.
An expired question becomes a failed task with a request to issue the original
command again. No background expiry timer or persistent reference database is
needed.

Native UI, command and voice callers must carry the displayed task and question
IDs into these APIs. After an answer they must use the newly returned status
token for approval. The legacy global window-controller `choose(number)` API
is not a substitute for this task-scoped flow. Ordinary dictation remains
outside these selectors.

## Native, CLI and speech use

The native task card displays each numbered choice, the unchanged operation,
and the remaining lifetime. Select a window or enter a short correction, then
use **Approve window action** on the new review. Choice buttons, approval and
discard retain the exact displayed task/question IDs. A stale click fails.
The native pane no longer offers another task's global window-controller list.

`hex agent status` displays the choices and the current commands:

```sh
hex agent answer-window <task-id> --question <question-id> "second"
hex agent approve-window-answer <task-id> --question <new-question-id>
hex agent cancel-window-question <task-id> --question <current-question-id>
```

An answer rotates the token, so read status again before approval. The legacy
`hex agent choose 2` now only selects from the active task's window question and
creates a review. It cannot execute a stale global choice. If a compound task
encounters ambiguity without a task question, repeat that window operation on
its own to get a fresh list.

Raw Actions speech resolves supported reference phrases before learned speech
corrections or model routing. Smart and wake command planning use the same
service path. `cancel this one` cancels only the pending window question. A
reference outside a current list is not consumed as a window choice, and no
spoken reference approves the resulting operation.

## Source and verification

Sources: `context/references.rs`, `service/window_dialogue.rs`, native target
preparation in `wm/targets.rs`, and the existing Service approval and Action
execution paths.

Automated checks exercise exact-token approval, reordered lists, bounded
selectors, previous/other references, low-confidence refusal, expiry, PID
reuse, Stop during native revalidation, stale task replacement, and expiry
while waiting for the shared input lease. Native-controller fixtures also
exercise prepared focus/move/maximize/minimize/previous targets and Stop or
expiry during the final native observation. Fixtures verify the executor
contract; they do not establish physical desktop or voice acceptance.

`src/rune_ui/agent/dialogue/tests.rs` checks the production task card at a
narrow width in both themes, including wrapped window titles, selection controls
and the separate approval step. Keyboard interaction reaches the answer field;
changing the question token or task clears its draft and stale error. The explicit
`render_native_window_question_light_and_dark` test writes
`tests/golden/rune-agent-window-question-{light,dark}.png` through the native
window compositor and requires a GPU or software Vulkan adapter. Inspect both
images alongside the full Rune suite when this question UI changes. These are
component fixtures, not an observation of a real window closing.

Broader task memory for project/file/page references and accessibility element
choices still needs its own stable identity and permissioned adapter. This
window implementation does not infer those references from similar titles or
reuse old accessibility element numbers.

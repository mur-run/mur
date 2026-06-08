# Hub Interrupt / Stop Control Design (v1)

## Goal

Give the MUR Hub chat a **Stop** control (button + Esc) that **cancels an agent's in-flight
generation** — letting the user halt a long or wrong reply instead of waiting it out. This is the
buildable, verified core of "barge-in" and the reusable substrate a future voice-activated barge-in
sits on.

## Why this (and why NOT voice in v1)

Grounding exploration + an ultra-review found true voice barge-in is blocked on prerequisites, and
that bundling voice into v1 is both broken and incoherent:

- **The Hub never speaks chat replies** — only the pet/mascot speaks (`pet_speak` → `VoicePlayer`,
  `mur-hub-gui/src-tauri/src/pet/mod.rs:325-337`), driven by the companion, not chat. So a chat "Stop"
  has no chat-voice to abort; silencing the unrelated mascot from a chat button is incoherent.
- **`voice_abort` is not reachable today.** `pet_speak` creates a *local* `VoicePlayer`
  (`pet/mod.rs:333`); nothing stores the instance, so no command can call its `abort()`. Exposing it
  needs new state plumbing — not "expose the existing method."
- **No desktop STT/VAD** — interruption can only be *manual* anyway.

So v1 cancels generation only. Voice abort, voice-activated barge-in, chat TTS, and STT/VAD are
deferred (see **Deferred** below).

## Scope

- **In:** a chat **Stop** affordance (button + Esc) that cancels in-flight generation via a
  **client-supplied task id** + the existing `tasks/cancel` RPC; preserving the partial streamed reply.
- **Out (deferred / other specs):**
  - **Voice abort** — needs `VoicePlayer` abort-handle plumbing (store the active player's
    `Arc<Notify>` in Tauri state, or make the speak loop bus-abortable) and belongs on the
    **pet/companion** surface where speech actually happens. Ship separately or once chat-TTS lands.
  - Desktop STT/VAD; voice-*activated* barge-in; Hub chat TTS; turn-taking state machine.

## Verified Current State

- **Cancel genuinely interrupts generation.** `run_sync_streaming` generates the id, registers
  `cancel_signals[id]`, and `tokio::select!`s the generation future against a cancel oneshot
  (`mur-agent-runtime/src/task_runner.rs:295,355-381`). `cancel(id)` fires it (`:394-405`). On cancel
  the generation future is dropped (Rust async cancellation → the LLM call aborts) and
  `message_send` returns `TaskOutcome::Cancelled(task)` (`message_send.rs:99-105`), which travels back
  over the streaming socket as the final result, ending the stream cleanly. **The mechanism is real
  and prompt.**
- **The id is generated *inside* the runner** (`task_runner.rs:295`) and only returned at the end —
  `message_send` never has it mid-flight. Hence the **client-supplied id** mechanism below.
- **`dial_message_streaming` has exactly one caller** (`mur-hub-gui/src-tauri/src/chat.rs:54`); with
  the client-supplied id it needs **no signature change**.

## Cancel-Mechanism Decision: client-supplied task id

The Hub **generates the task id** and passes it in `message/send` params; the runner honors it; the
Hub cancels with the id it already holds.

| Option | Verdict |
|---|---|
| **Client-supplied id** | **Chosen.** No new notification; **no `dial_message_streaming` change**; **no race** (Hub knows the id before the stream starts). One additive runtime touch. |
| `task/started` notification + `on_task_id` callback | Rejected — needs runner→message_send id surfacing, a new dial callback, and a cancel-pending race window. Strictly more moving parts. |
| Close the streaming socket | Rejected — agent keeps generating server-side; fake cancel. |

## Architecture

```
Send:   ChatTab generates taskId (uuid) → agent_chat_send(name, text, taskId)
        chat.rs: registry[name] = taskId ; dial_message_streaming(params{ ..., task_id: taskId })
        runtime: message_send reads p["task_id"] → TaskSpec.task_id
                 run_sync_streaming uses it (else generates) ; message/delta… (existing)
Stop:   user clicks Stop / Esc (only while busy)
        → agent_chat_cancel(name) → registry[name] → dial tasks/cancel{ id } on a NEW connection
              → TaskRunner.cancel(id) → select! fires → TaskOutcome::Cancelled
              → message_send returns final result on the streaming socket → dial returns
        → ChatTab commits its OWN streamed buffer as the agent message + "stopped" marker
```

The streaming socket is never force-closed; cancel makes the agent finish early and send its final
result, which the existing read loop consumes and returns. Cancel is dialed on a **separate**
connection so it doesn't fight the in-progress read.

## Components

| Unit | New/Change | Responsibility | Depends on |
|---|---|---|---|
| `TaskSpec.task_id: Option<String>` (`task_runner.rs`) | Change (additive) | Optional caller-supplied id. | — |
| `run_sync_streaming` id line (`task_runner.rs:295`) | Change | `let id = spec.task_id.clone().unwrap_or_else(\|\| format!("task-{}", Uuid::now_v7()));` | TaskSpec.task_id |
| `message_send` params (`message_send.rs:60-71`) | Change (additive) | Read `p["task_id"]` into `TaskSpec.task_id` (ignored if absent → back-compatible). | — |
| in-flight registry (`chat.rs`) | New | `Mutex<HashMap<agent, task_id>>`; set in `agent_chat_send`, cleared on completion. | — |
| `agent_chat_send` (`chat.rs`) | Change | Accept `taskId` param; store in registry; pass `task_id` in `message/send` params. | registry |
| `agent_chat_cancel` Tauri command (`chat.rs`) | New | Look up `registry[name]`; dial `tasks/cancel{ id }` (`DialMode::RequireRunning`); no-op if absent. | `tasks/cancel`, registry |
| `ChatTab.tsx` | Change | Generate `taskId` per send; pass to `agent_chat_send`; **Stop** button while `busy`; Esc handler; on Stop call `agent_chat_cancel` and **commit the local streamed buffer** as the reply with a "stopped" marker. | the commands |

## Partial-Reply Preservation (M1)

On cancel, the partial text already lives in ChatTab's `streamingRef.current` (streamed via
`message/delta` before cancel). So ChatTab must **commit its own streamed buffer** as the agent
message on Stop — it must **not** depend on the Cancelled task's body (which may be empty). Concretely,
on Stop: snapshot `streamingRef.current`, push it as an `agent` message tagged "stopped", then clear
streaming state. The subsequent `dial` return (final Cancelled result) is then ignored for content.

## Error Handling

| Condition | Behavior |
|---|---|
| Stop pressed but no id in registry (extremely narrow — id is set synchronously in `agent_chat_send` before dialing) | `agent_chat_cancel` no-ops; ChatTab still commits the partial + stops the spinner |
| Cancel after task already finished | `tasks/cancel` → `TaskNotFound` → swallowed (reply already delivered) |
| Cancel dial fails (agent died) | streaming read hits EOF → `dial` errors → ChatTab shows partial + error line |
| Duplicate / rapid Stop | idempotent: second `tasks/cancel{id}` → `TaskNotFound` swallowed |
| Client supplies a colliding id | single-user Hub + uuid → negligible; runner is last-writer for `cancel_signals[id]` |

## Testing

- **Runtime** (`task_runner`): `run_sync_streaming` with `spec.task_id = Some("task-x")` uses that id
  (assert the registered `cancel_signals` key / returned `task.id`); with `None` it still generates one.
- **Runtime** (`message_send`): `p["task_id"]` flows into `TaskSpec.task_id`; absent → `None`
  (back-compatible).
- **Cancel path** (existing/extended): a task cancelled by id yields `TaskOutcome::Cancelled` promptly
  (mirror the existing cancel test).
- **`chat.rs`**: `agent_chat_send` stores `registry[name]=taskId`; `agent_chat_cancel` dials
  `tasks/cancel` with it; no-ops cleanly when absent; registry cleared on completion.
- **`ChatTab`** (pure-logic where feasible — this project has **no DOM test harness**, only Vitest
  node tests): Stop visible only while `busy`; Stop commits `streamingRef` content as the reply with a
  "stopped" marker and stops the spinner; a fresh `taskId` is generated per send.

## File Boundaries / Coordination

- All runtime edits are small and additive (`TaskSpec` field, one `unwrap_or_else`, one param read).
  No file split needed.
- **Coordination:** `task_runner.rs` + `message_send.rs` are in `mur-agent-runtime` (touched by
  in-flight Phase 3/4) and `ChatTab.tsx` is also touched by the multi-agent rail plan
  (`2026-06-08-hub-multiagent-conversation-rail.md`). Changes are additive; sequence after those land
  or merge carefully.

## Deferred — Voice Abort (separate piece)

When wanted, `voice_abort` belongs on the **pet/companion** surface (the only place speech occurs) and
requires reachability plumbing that does **not** exist today:
- Store the active `VoicePlayer`'s `Arc<Notify>` (add a `VoicePlayer::abort_handle()` getter) in a
  Tauri-managed `VoiceAbortState` keyed by agent, set at `pet_speak` start and cleared on `voice.ended`;
  `voice_abort(agent)` signals it. (Alternative: make the speak loop subscribe to a bus abort event.)
- This is independent of generation-cancel and reuses none of v1's chat plumbing, so deferring costs
  v1 nothing.

## Future Seams (no v1 work)

- **Voice-activated barge-in**: when desktop STT/VAD exists, a detected-speech event calls the same
  `agent_chat_cancel` (+ a future `voice_abort`) — no new interruption plumbing.
- **Chat TTS**: when the Hub speaks replies, the chat Stop should *also* abort that playback — at which
  point voice abort folds naturally into this same Stop control.

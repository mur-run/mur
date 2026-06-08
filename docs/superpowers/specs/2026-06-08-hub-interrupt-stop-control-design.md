# Hub Interrupt / Stop Control Design (v1)

## Goal

Give the MUR Hub a single **Stop** control (button + Esc) that makes a busy agent stop: it **aborts
active TTS playback** and **cancels in-flight generation**. This is the manual, buildable core of
"barge-in" — table-stakes for chat, and the reusable substrate a future voice-activated barge-in
sits on.

## Why this, not "voice barge-in"

Grounding exploration found true voice barge-in is blocked on prerequisites that don't exist on
desktop:
- **The Hub never speaks chat replies** — only the pet/mascot speaks (`pet_speak` →
  `VoicePlayer`), driven by the companion, not chat (`mur-hub-gui/src-tauri/src/pet/mod.rs:321-337`;
  `chat.rs` has no speech).
- **No desktop STT/VAD** — `is_mic_busy()`/`is_focus_active()` exist (`mur-gui-core/src/voice/dnd.rs`)
  but there is zero speech recognition; STT is iOS-only. The desktop **cannot detect the user
  speaking**, so interruption can only be *manual*.
- **In-flight generation can't be cancelled** from the Hub — `dial_message_streaming`
  (`mur-core/src/a2a_dial.rs:174-245`) is a blocking socket read with no cancel path.

So v1 builds the manual Stop. Voice-*activated* barge-in, chat TTS, and STT/VAD are deferred.

## Scope

- **In:** one Stop affordance (button + Esc); `voice_abort` Tauri command (exposes existing
  `VoicePlayer::abort()`); additive `task/started` notification carrying the task id; an `on_task_id`
  callback in `dial_message_streaming`; `agent_chat_cancel` Tauri command (dials existing
  `tasks/cancel`); cancel-pending race handling.
- **Out (deferred / other specs):** desktop STT/VAD; voice-*activated* barge-in; Hub chat TTS
  (speaking replies); turn-taking / voice state machine.

## Current State (verified)

- **`VoicePlayer::abort()` already exists** (`mur-gui-core/src/voice/synth.rs:46-49`): non-blocking
  `Notify`; the speak loop checks it every 200ms and stops playback + lipsync, emitting `voice.ended`.
  **Not exposed as a Tauri command** — only `pet_speak` is wired (`pet/mod.rs:335`).
- **`tasks/cancel` RPC exists** (`mur-agent-runtime/src/protocol/methods/tasks.rs:32-53`):
  `TasksCancelHandler` → `TaskRunner.cancel(id)`; needs the task id.
- **`dial_message_streaming`** reads `message/delta` + `tool/approval_needed` + the final `result`
  (id-matched); **the task id is only in the final result** — not surfaced mid-stream
  (`a2a_dial.rs:203-245`).
- **`message_send.rs`** emits `message/delta` during generation
  (`mur-agent-runtime/src/protocol/methods/message_send.rs:84`) but **no early id notification**.
- **`ChatTab.tsx`** threads `taskIdRef` from the *previous* turn's result; during a turn it has no id.

## Cancel-Mechanism Decision

| Option | Verdict |
|---|---|
| **A — additive `task/started` notification + existing `tasks/cancel`** | **Chosen.** Runtime emits the task id at stream start; Hub stores it; cancel reuses the existing handler. Additive emit (low collision risk), correct, agent stops cleanly and sends its final result so the stream ends gracefully. |
| B — close the streaming socket | Rejected — agent keeps generating server-side (burns tokens); fake cancel. |
| C — new `tasks/cancelCurrent` (no id) | Rejected — needs the runtime to track "current task" + a brand-new method; more invasive and less precise than A. |

## Architecture

```
Send:   ChatTab → agent_chat_send → dial_message_streaming(..., on_task_id, on_delta, on_hitl)
        runtime: create task → emit task/started{id}  ──► on_task_id ──► chat.rs registry (agent→id)
                 → message/delta… (existing)                                  └─► emit to UI (busy)
Stop:   user clicks Stop / presses Esc (only while busy)
        ├─ voice_abort(agent)       → VoicePlayer::abort()      → "voice.interrupted"
        └─ agent_chat_cancel(agent) → dial tasks/cancel{id} on a NEW connection
                                       → TaskRunner.cancel(id) → generation stops
                                       → agent sends final result on the streaming socket
                                       → dial_message_streaming returns
        ChatTab finalizes: keep partial streamed text, mark "stopped"
```

The streaming socket is **not** force-closed; cancelling makes the agent finish early and send its
final result, which the existing read loop consumes and returns. Cancel is dialed on a **separate**
connection so it doesn't fight the in-progress read.

## Components

| Unit | New/Change | Responsibility | Depends on |
|---|---|---|---|
| `voice_abort` command (`mur-hub-gui/src-tauri/src/voice.rs` or near `pet/mod.rs`) | New | Resolve the agent's `VoicePlayer`/bus and call `abort()`; emit `voice.interrupted`. | existing `VoicePlayer::abort()` |
| `task/started` emit (`message_send.rs`) | Change (additive) | After the task is created and before the first delta, send one `task/started` notification `{ id }`. | runner task id |
| `on_task_id` (`dial_message_streaming`, `a2a_dial.rs`) | Change | New `on_task_id: impl FnMut(&str)` param; fire on a `task/started` notification. | — |
| in-flight registry (`chat.rs`) | Change | `Mutex<HashMap<agent, task_id>>`; set on `on_task_id`, clear on completion. | — |
| `agent_chat_cancel` command (`chat.rs`) | New | Look up the agent's in-flight id; dial `tasks/cancel{ id }` (`DialMode::RequireRunning`). | `tasks/cancel`, registry |
| `ChatTab.tsx` | Change | Stop button while `busy`; Esc handler; calls `agent_chat_cancel` + `voice_abort`; `cancelPending` race; preserve partial reply + "stopped" line. | the two commands |

`voice_abort` is independent of the cancel pieces and can ship on its own.

## Data Flow Detail — cancel-pending race

`agent_chat_send` may not yet have received `task/started` when the user hits Stop. Handle in the UI:

```
Stop pressed while busy:
  always: invoke("voice_abort", { name })
  if currentTaskId known: invoke("agent_chat_cancel", { name })
  else: set cancelPending = true
On task/started event (or when agent_chat_send resolves with an id):
  if cancelPending: invoke("agent_chat_cancel", { name }); cancelPending = false
```

The Hub also needs the id in the UI to drive this. Two options, pick one in the plan: (a) surface
`task/started` as a Tauri event the UI listens to, or (b) keep the id only in `chat.rs` and have
`agent_chat_cancel` itself consult the registry (UI just calls cancel; backend no-ops if no id yet,
and the UI sets `cancelPending` to retry on completion). **Prefer (b)** — less UI state, the registry
is the single source of truth; `cancelPending` only guards the "Stop pressed, nothing to cancel yet"
window.

## Error Handling

| Condition | Behavior |
|---|---|
| Stop before `task/started` arrives | abort TTS now; `agent_chat_cancel` no-ops (no id in registry); UI sets `cancelPending`, retries on send-resolution |
| Cancel after task already finished | `tasks/cancel` → `TaskNotFound` → swallowed (reply already delivered) |
| `voice_abort` with nothing speaking | `abort()` is a no-op `Notify` → harmless |
| Cancel dial fails (agent died) | streaming read hits EOF → `dial` errors → ChatTab shows partial + error line |
| Multiple rapid Stops | idempotent: abort is a no-op when idle; cancel of an already-cancelled id → `TaskNotFound` swallowed |

## Testing

- **Runtime** (`message_send`): emits `task/started` with the task id *before* any `message/delta`
  (handler/integration test).
- **`dial_message_streaming`**: `on_task_id` fires once when a `task/started` notification is read,
  before deltas; final result still returns normally (fake-socket test mirroring existing dial tests).
- **`chat.rs`**: registry set on `on_task_id`, cleared on completion; `agent_chat_cancel` dials
  `tasks/cancel` with the stored id; no-ops cleanly when the registry has no id.
- **`voice_abort`**: invokes `VoicePlayer::abort()` and emits `voice.interrupted`.
- **`ChatTab`** (pure-logic where possible — this project has no DOM test harness): Stop visible only
  while `busy`; triggers both commands; `cancelPending` set when no id yet and fired on resolution;
  partial streamed text retained with a "stopped" marker.

## File Boundaries / Coordination

- `a2a_dial.rs` is 401 lines; adding one callback parameter + one notification branch is in-scope and
  small. No split needed.
- **Coordination:** `message_send.rs` is in `mur-agent-runtime` (touched by in-flight Phase 3/4) and
  `ChatTab.tsx` is also touched by the multi-agent rail plan
  (`2026-06-08-hub-multiagent-conversation-rail.md`). All changes here are additive (a new
  notification, a new Stop button), but sequence them after those land or merge carefully.

## Future Seams (no v1 work)

- **Voice-activated barge-in**: when desktop STT/VAD exists, a detected user-speech event simply calls
  the same `voice_abort` + `agent_chat_cancel` — no new interruption plumbing.
- **Chat TTS**: when the Hub speaks replies, `voice_abort` already silences it; Stop needs no change.

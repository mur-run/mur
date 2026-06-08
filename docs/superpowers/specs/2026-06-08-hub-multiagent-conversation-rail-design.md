# Hub Multi-Agent Conversation Rail Design (v1)

## Goal

Give the MUR Hub a **vertical conversation rail** so the user can hold several live agent
conversations at once — open multiple agents, switch between them instantly, and never miss activity
(streaming replies, HITL approvals, completions) on an agent they aren't currently looking at. The
Hub today is strictly single-agent; this makes it multi-conversation.

## Scope

- **In:**
  - A persistent **Conversations surface**: a vertical rail of open conversations + one active
    conversation panel.
  - **Buffered multi-conversation state**: every open conversation accumulates its stream even when
    not active, so switching is instant and nothing is lost.
  - **Attention model**: rail items show status + an attention badge (pending HITL / unread reply);
    background activity also fires the existing Phase-2 notification (budget-respecting).
  - **Separation of concerns**: extract chat out of the per-agent Configuration detail panel into the
    rail-driven conversation panel.
- **Out (deferred / other specs):**
  - Split / concurrent "watch 2–3 side by side" view → **v2** (state model here makes it a
    layout-only addition).
  - Voice barge-in → separate spec.
  - Any inter-agent **routing/orchestration** → that is **Commander's** domain. This spec only leaves
    a clean seam (a generic "conversational target" notion); it builds no orchestration.
  - Per-agent event channels / event-model refactor — the current broadcast-filtered-by-name model is
    sufficient at single-user scale.

## Commander Boundary (why this stays UI-only)

Commander is MUR's designated orchestration + governance + audit engine. Building inter-agent routing
into the Hub rail would create a second orchestration mechanism that either bypasses Commander's
governance (an audit hole) or duplicates it. So the layering is fixed: **Commander = orchestration
engine; Hub rail = local human interface** (observe + converse). In v1 *the user is the router*. The
rail models a lane as a generic conversational target so a future Commander view can slot in without
rework.

## Current State (verified)

- **Single-agent today.** `AgentContext.tsx` holds `selectedAgent: string | null`
  (`mur-hub-gui/ui/src/context/AgentContext.tsx:6-18`); `DetailPanel.tsx` (801 lines) renders one
  agent's tabs (chat, persona, style, behavior, skills, MCP, permissions, inbox, mobile, memory).
- **Chat/stream/HITL already concurrent-capable.** `agent_chat_send` →
  `dial_message_streaming` emits `chat-delta` and `hitl-approval-needed` events **broadcast globally,
  filtered by agent name in the UI** (`mur-hub-gui/src-tauri/src/chat.rs`, `ChatTab.tsx:68-89`). So
  multiple agents streaming at once already works; `ChatTab` already filters by an `agentName` prop.
- **Status already flows.** `discovery.rs` emits `agents-updated` (5s scan); the sidecar supervisor
  emits `runtime-status-changed` (`mur-hub-gui/src-tauri/src/lib.rs:143-156`).
- **Implication:** v1 needs **no `mur-core`/runtime changes** — it is a Hub UI + state change.

## Architecture

```
┌───────────────────────── Conversations surface (new) ─────────────────────────┐
│  ConversationRail (left)        │            ConversationPanel (right)         │
│  ┌──────────────┐               │   active agent's ChatTab (existing, reused)  │
│  │ ● research ▲ │ ← active      │   ┌────────────────────────────────────────┐ │
│  │ ◑ writer  ② │ ← 2 unread    │   │ streamed messages (from buffer)         │ │
│  │ ● ops  ⚠HITL │ ← needs you   │   │ HitlCard (if pending)                   │ │
│  │ + open…      │               │   │ input box                               │ │
│  └──────────────┘               │   └────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────────────────────┘
        ▲ open/close/focus                        ▲ chat-delta / hitl events
        │                                          │ (global, filtered by agent name)
   conversationStore  ←──────────── buffers ALL open conversations, not just active
        │
   attention → Phase-2 notification (budgeted) for background HITL / completion
```

Two **independent** concepts, deliberately separated:
- **Active conversation** — driven by the rail (this spec).
- **Selected agent for configuration** — the existing grid → `DetailPanel` flow (unchanged in
  purpose). `selectedAgent` stays for config; conversations get their own state.

## Components

| Unit | New/Change | Responsibility | Depends on |
|---|---|---|---|
| `ConversationContext.tsx` (new, dedicated — **not** folded into `AgentContext`) | New | `openConversations: Map<agentName, ConversationState>` + `activeConversation: string \| null`; actions `open`/`close`/`focus`; ingest `chat-delta`/HITL into the right buffer; attention flags. Reads agent list/status from `AgentContext`. | `AgentContext`, Tauri `listen` events |
| `ConversationState` (type) | New | `{ agent, messages, streaming, pendingHitl: HitlRequest[], unread: number, attention: "none"\|"unread"\|"hitl" }` | — |
| `ConversationRail.tsx` | New | One item per open conversation: status dot (from runtime status) + attention badge + close button; click → `focus`. | `conversationStore`, runtime status |
| `ConversationPanel.tsx` | New | Thin wrapper rendering the **existing `ChatTab`** for `activeConversation`. | `ChatTab.tsx` (reused) |
| `ConversationsView.tsx` | New | Layout: rail (left) + panel (right); the new top-level surface. | the three above |
| `AgentContext.tsx` | Unchanged | Keeps `selectedAgent` for the config detail panel; conversation state lives in `ConversationContext`. | — |
| `DetailPanel.tsx` | Change | Remove the chat tab (config-only now); a "Chat" affordance calls `conversationStore.open(agent)`. Reduces this 801-line file. | `conversationStore` |
| `DashboardApp.tsx` | Change | Mount `ConversationsView`; grid card "Chat" action opens-or-focuses a conversation. | `conversationStore` |
| attention → notification wiring (`src-tauri/src/lib.rs` or UI) | Change | Background HITL/completion sets the rail attention flag **and** emits the existing Phase-2 notification (budgeted). | Phase-2 notification path |

Each unit is independently testable: the store is pure state + event-ingest logic; the rail and panel
are presentational; the notification wiring is a thin adapter.

## Data Flow

```
Open:        grid/detail "Chat" → store.open(agent) → rail item appears → becomes active
Send/stream: ConversationPanel(active) → agent_chat_send → chat-delta (filtered by agent)
             → store buffers into that agent's ConversationState (even when NOT active)
Background:  chat-delta / hitl-approval-needed for a non-active agent
             → store buffers + sets attention ("unread" or "hitl")
             → on HITL or task-complete: also emit Phase-2 notification (budgeted)
Switch:      click rail item → activeConversation = agent → panel shows buffer, clears unread
HITL:        HitlCard in the panel → existing agent_hitl_respond(name, hitl_id, allow, reason)
Close:       rail item × → store.close(agent) (history dropped; reopening starts fresh)
```

Because the store buffers **all** open conversations, switching is instant and background streams are
never lost.

## Error Handling

| Condition | Behavior |
|---|---|
| Agent stops/crashes mid-conversation (`runtime-status-changed` → error) | rail dot turns error; conversation stays open with history; input disabled + "agent stopped — restart?" affordance |
| Dial/send failure | inline error in that conversation panel (reuse `ChatTab` error display); other conversations unaffected |
| Agent removed from disk while open (`agents-updated` drops it) | rail item marked stale; close-only; no send |
| HITL timeout on a background agent | existing auto-deny/timeout applies; attention flag clears; a system line is appended to that buffer |
| Same agent opened twice | `open` is idempotent — focuses the existing conversation, never duplicates |

## Testing

- **`conversationStore`** (unit): `open`/`close`/`focus`; idempotent `open`; a background `chat-delta`
  buffers into the correct conversation; attention set for non-active, cleared on `focus`; unread
  count increments then resets.
- **Concurrent streams** (unit): interleaved `chat-delta` for agents A and B land in separate buffers
  with no cross-contamination — guards the global-event/name-filter model.
- **`ConversationRail`** (component): one item per open conversation; status dot reflects runtime
  status; attention badge shown when `pendingHitl` non-empty; close removes the item.
- **`ConversationPanel`** (component): renders the active agent's `ChatTab`; switching active agent
  swaps the visible buffer.
- **Notification integration**: a background HITL fires exactly one budgeted Phase-2 notification (not
  one per delta).
- **`DetailPanel`**: chat tab removed; remaining config tabs intact; "Chat" affordance calls
  `store.open`.

## File Boundaries

- `DetailPanel.tsx` is already **801 lines** (over CLAUDE.md rule 4's 800 limit). Removing the chat tab
  + extracting it into `ConversationPanel`/`ConversationsView` is a targeted, in-scope improvement that
  brings it back under the limit. No unrelated refactoring.
- New components are small and single-purpose (`ConversationRail`, `ConversationPanel`,
  `ConversationsView`); the store holds the only non-trivial logic and is unit-tested in isolation.

## Future Seams (no v1 work)

- **v2 split view**: render N `ConversationPanel`s from the same store in a flex row — pure layout, no
  state change, because buffers are already per-agent and concurrency-safe.
- **Commander view**: a rail lane is a generic conversational target; a Commander-orchestrated
  task/team could become a lane later, surfacing governance/audit events through the same attention
  model and the (already generic) HITL card.

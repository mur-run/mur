# Unified Chat Redesign — one substrate, two surfaces, one search

**Date:** 2026-08-16  
**Status:** Approved design, pre-implementation  
**Scope:** `mur-common`, `mur-channel`, `mur-core` TUI/mobile, `mur-hub-gui`, `mur-daemon`, `mur-mobile-sdk`, deprecated `mur-agent-gui`

## 1. Decision

MUR will use one durable Channel substrate while presenting two user concepts:

- **Chats** — conversations between a human and one or more agents.
- **Work** — Fleet and Workflow executions.

Users do not need to understand Channel. The term remains only in advanced CLI,
debugging, storage, and HITL plumbing. Chats and Work never share an inbox, but a
single global search can find both and displays results in clearly separated groups.

The design principle is: **one substrate, two surfaces, one search**.

## 2. Problem

The durable data is increasingly converging on `~/.mur/channels/`, but every
surface presents and binds it differently:

- Hub Chats lists agents, while its popout exposes individual channels and raw IDs.
- Hub Work/Fleets separately exposes execution channels.
- TUI `/sessions` and `/channels` list the same per-agent channel data with different names.
- Mobile is channel-first and mixes conversations with execution activity.
- Hub's Mobile tab still reads the `mobile-events.jsonl` mirror.
- The deprecated Agent GUI Companion has its own inbox-shaped presentation.
- `mur chat` searches the separate ambient conversation archive, not Channel events.

Surface-by-surface, with the code an implementer starts from:

| Surface | Today | Code |
|---|---|---|
| murmur TUI | `/sessions` and `/channels` both call `persist::list_recent` — two names, one list | `mur-core/src/cmd/agent/cli/mod.rs` |
| Hub Chats page | one row per agent; inspector shows the raw channel UUID; every send re-resolves `latest_for_agent` | `chats/chatList.ts`, `src-tauri/src/chat.rs` |
| Hub popout window | side rail of truncated UUIDs + FLEET section; badges show turn totals | `chat/ChatChannelRail.tsx` |
| Hub Mobile tab | reads the `mobile-events.jsonl` mirror, renders raw JSON errors as bubbles | `MobileTab.tsx` |
| Phone app | channel-first list via v4a `list_channels()` | `mur-mobile-sdk` |

Titles are useless today because `create_for_agent` names every chat channel
`"chat with {agent}"` (`mur-channel/src/service.rs`), which is why UUIDs leak.

This causes four classes of failure:

1. Users cannot predict where a conversation or execution will appear.
2. UUIDs, turn totals, and implementation terms leak into product UI.
3. Different surfaces independently group, classify, and bind the same data.
4. Search semantics differ by surface and do not cover Channel message content.

## 3. Goals

1. Give users one obvious place for agent conversations: Chats.
2. Keep operational Fleet/Workflow timelines out of the chat inbox.
3. Make conversation history, continuation, unread state, and cross-device binding consistent.
4. Provide one global, grouped search across agents, conversations, messages, and work runs.
5. Make Hub, Mobile, and TUI consume the same summary and search contracts.
6. Migrate legacy data without mutating it during reads or relying on frontend heuristics.

## 4. Non-goals

- Do not merge the ambient archive behind `mur chat` with Channel storage.
- Do not make `mur channel` a user-facing synonym for Chats.
- Do not put Fleet/Workflow event streams in the Chats inbox.
- Do not introduce LLM-generated titles in this phase.
- Do not add a second Companion inbox.
- Do not remove advanced `/channels` inspection and follow behavior from the TUI.

## 5. Domain model

### 5.1 Persist only stable purpose

`Channel` gains an optional, additive purpose field:

```rust
pub enum ChannelPurpose {
    Conversation,
    FleetRun,
    WorkflowRun,
}

#[serde(default)]
pub purpose: Option<ChannelPurpose>,
```

New channels must write `Some(purpose)`. `None` means legacy and must never be
treated as an explicit Conversation default.

Purpose is intentionally smaller than a flat Direct/Group/Fleet/Workflow/Companion
enum. It stores why a channel exists, not every way a UI may render it.

### 5.2 Derived presentation concepts

- **Direct** — a Conversation with exactly one Agent participant.
- **Group** — a Conversation with two or more Agent participants.
- **Companion** — an Agent-authored proactive message origin, not a Channel purpose.
- **HITL** — an attention state derived from events, not a Channel purpose.

Participant counts include only `ChannelActor::Agent`; the local human participant
does not make a Direct conversation a two-agent conversation.

New Conversation channels require at least one Agent participant. A malformed legacy
Conversation with zero Agent participants remains discoverable through diagnostics
and advanced Channel tools but is not silently presented as a Direct chat.

### 5.3 Legacy classification

One pure `effective_purpose(&Channel)` function owns temporary inference:

1. Explicit `purpose` always wins.
2. Stable `fleet-*` ID implies FleetRun.
3. Existing workflow creation metadata/title convention implies WorkflowRun.
4. Otherwise infer Conversation.

Clients never run these rules. `mur-channel` resolves purpose before producing a
summary. Reading a summary never persists an inferred value.

An explicit `mur channel backfill-purpose` command performs migration with
`--dry-run`, bounded batches, and `--apply`. Corrections update Channel metadata
atomically and rebuild the disposable read-model row.

## 6. Information architecture

### 6.1 Hub navigation

- **Home** — an attention projection: pending approvals, unread conversations,
  recently completed work. Every card deep-links to Chats or Work.
- **Chats** — the only conversation inbox and composer.
- **Work** — FleetRun and WorkflowRun lists and execution timelines.
- **Library** — reusable definitions and configuration: Agents, Fleets, Skills,
  Models, and settings.

Agent cards in Library retain a **Chat** shortcut. Fleet definitions retain a
**Run** shortcut. These are entry points into Chats or Work, not separate transcript UIs.

### 6.2 Mobile navigation

Mobile exposes **Chats** and **Work** as separate primary destinations and opens
Chats by default. A Work badge reports pending approvals without inserting runs into
the chat list.

### 6.3 TUI vocabulary

- `/chats` lists and switches the current agent's Conversation history.
- `/sessions` is a compatibility alias for `/chats` during migration.
- `/clear` starts an unbound draft; the first send lazily creates a Conversation.
- `/channels` remains advanced plumbing, lists every purpose, and retains follow mode.
- The status line shows the conversation title, never the raw Channel ID.

The top-level `mur chat` ambient archive command remains unchanged.

### 6.4 Deprecated Agent GUI (follow-up scope)

No new independent Companion inbox is built. Proactive Companion messages append to
the relevant active Conversation and carry additive origin metadata for optional UI
styling — that routing rule is part of this design. Migrating the deprecated Agent
GUI to be a thin client of the Conversation API is **follow-up scope**: it is not
part of Phase 1–3 acceptance and lands with the Companion convergence follow-up.

## 7. Chats experience

### 7.1 Inbox

Chats is agent-first because users normally begin with “who do I want to talk to?”
Each row contains:

- Agent or group avatar and display name.
- Last-message preview.
- Relative activity time.
- True unread count.
- Pending in-conversation HITL badge, when applicable.

Ordering is: pending HITL, unread, then `updated_at` descending. Agents with no
conversation remain visible with a muted “No conversations yet” row that starts a
new chat.

### 7.2 Active conversation and history

Each Direct agent has one active Conversation: the most recently updated applicable
Conversation. Opening the agent continues that Conversation.

The chat header includes **History**. Its drawer lists title, time, preview, and turn
count. Selecting history opens it without changing its active status. Sending from
that view continues the selected Conversation and naturally makes it latest.

Uniform rule: **viewing never resumes; sending resumes**.

### 7.3 New chat and lazy creation

“New chat” clears the bound Conversation ID but creates nothing on disk. The first
successful send creates the Channel, preventing abandoned empty history entries.

For one Agent, creation produces a Direct conversation. The UI may display existing
multi-agent Conversation channels as Groups. Creating and routing new Group chats is
deferred to a dedicated follow-up design because responder selection and `@agent`
routing are runtime behavior, not merely interface unification.

### 7.4 Stable binding across surfaces

A chat surface binds to the exact Conversation ID it displays and passes that ID to
the persistence path. It must not call `latest_for_agent` again on every send.

If another surface creates a newer Conversation, the bound window keeps its current
thread and shows a non-blocking notice: “A newer conversation exists — open it.”
Sending into the older bound thread is intentional continuation and makes it latest.

Hub, TUI, and Mobile all follow this rule.

### 7.5 Titles

The first non-empty human message supplies a deterministic title, truncated at a
shared configurable character limit. Attachment-only fallback is
`{agent display name} · {local date}`. Users may rename a conversation later.

Legacy default titles such as `chat with {agent}` are backfilled only by the explicit
migration command, after purpose classification. Summary reads never rewrite titles.

## 8. Work experience

Work contains FleetRun and WorkflowRun channels. A common card shows:

- Goal or title.
- Fleet/Workflow badge.
- Current state and step/iteration.
- Participating agents.
- Pending HITL state.
- Last activity time.

The detail view is an execution timeline for state changes, delegated work, tool
calls/results, approvals, artifacts, and completion. It is not rendered as alternating
chat bubbles unless an event is genuinely a human/agent message.

Fleet definitions live in Library; each execution is a FleetRun in Work. Definitions
and runs are never represented as the same object.

## 9. Home attention projection

Home owns no separate conversation store. It queries the same read model and presents:

1. Needs approval.
2. Unread conversations.
3. Failed or recently completed work.

Cards use the destination's vocabulary and route to the exact Conversation or Run.
Home never creates a third heuristic category such as “work-like channel.”

## 10. Search

### 10.1 Entry points

- Global `⌘K` searches Agents, Conversations, Messages, Work runs, and commands.
- Chats search is scoped to Conversations and their messages.
- Work search is scoped to FleetRun and WorkflowRun.
- TUI `/chats <text>` searches current-agent Conversation titles and messages.

### 10.2 Result contract

Global results are grouped, never interleaved as one unexplained list. Every result
includes type, title, Agent/Fleet context, relative time, and a highlighted snippet.

Selecting a message hit opens the exact Conversation and scrolls to the event.
Selecting an old hit does not resume it and does not mark later unread events as read.

### 10.3 Search ownership

`mur-channel` owns Channel search. It projects searchable message/event text into a
rebuildable SQLite search read model alongside Channel summaries. Hub, Mobile, and TUI
call the same API. The ambient archive's `ChatAction::Search` is not reused.

## 11. Unified read model and APIs

The existing disposable Channel SQLite index expands to hold or derive:

- Channel purpose.
- Agent participant IDs.
- Display title and last-message preview.
- Updated time and event/turn counts.
- Highest event sequence.
- Last-read sequence.
- Unread count.
- Pending HITL state.
- Fleet/Workflow progress summary.
- Searchable event text and snippets.

`mur-channel` exposes three product-level contracts:

- `list_conversations(options) -> Vec<ConversationSummary>`
- `list_runs(options) -> Vec<RunSummary>`
- `search(query, scope) -> SearchResults`

`list_conversations` options are an explicit struct, not ad-hoc parameters, so
"one fact, three renderings" cannot decay into one API, three interpretations:

```rust
pub struct ConversationQuery {
    pub agent: Option<String>, // None = all agents
    pub active_only: bool,     // true = latest conversation per agent
}
```

Callers: Hub Chats rows and daemon `ChannelQuery("list")` use
`{ agent: None, active_only: true }` (one row per agent, no client-side
grouping); the History drawer and TUI `/chats` use
`{ agent: Some(x), active_only: false }`.

All grouping, classification, unread calculation, and stable ordering live behind
these contracts. Frontends render summaries and do not inspect ID prefixes.

## 12. Unread and attention semantics

Unread count is the number of unread human-visible events after `last_read_seq`, not
the total turn count. Tool deltas and internal state events do not inflate chat unread.

A surface advances the monotonic watermark only when:

1. The window/view is focused.
2. The exact Conversation is visible.
3. The tail has actually been rendered.

Opening an old search hit does not jump the watermark to the end. The single-user,
shared-home model uses one watermark per Channel so reading on one connected surface
clears it everywhere. Writes use the existing exclusive locking discipline.

Conversation HITL badges Chats. Fleet/Workflow HITL badges Work and Home, never Chats.

## 13. Mobile and Companion convergence

Mobile turns already persist into Channels. Hub's Mobile tab switches to the unified
Conversation API and is then removed. `mobile-events.jsonl` remains a compatibility
mirror for one release after all readers migrate; its writer is removed afterward.

Companion proactive messages append as normal Agent messages to the active Direct
Conversation. Optional payload origin metadata may render a subtle “Proactive” label,
but does not create a separate channel, inbox, or unread system. The Agent-GUI /
Companion client migration itself is follow-up scope (§6.4); the routing rule above
binds all new work immediately.

## 14. Error handling

- Unknown legacy purpose: resolve centrally for display and report in migration dry-run.
- Invalid Conversation with no Agent: omit from Chats, retain in advanced diagnostics.
- Missing/corrupt index: rebuild from Channel manifests and event logs.
- Search index unavailable: show title/participant results and report content search as
  temporarily unavailable; never return silently incomplete mixed results.
- Bound Conversation deleted: preserve the unsent draft and offer to start a new chat.
- Concurrent watermark writes: keep the maximum sequence.
- Purpose correction: atomically update metadata and read model; never rewrite event history.
- Mobile mirror mismatch during transition: Channel is authoritative.

## 15. Migration and rollout

### Phase 1 — model and contracts

1. Add optional `ChannelPurpose` and compatibility tests.
2. Make every creation path write explicit purpose.
3. Add centralized `effective_purpose` and summary/search contracts.
4. Add purpose, previews, attention, and search to the rebuildable index.

No navigation changes ship before all surfaces can consume these contracts.

### Phase 2 — binding and invisible convergence

1. Switch Hub, TUI, daemon/mobile, and Agent GUI readers to unified APIs.
2. Bind Hub and Mobile sends to explicit Conversation IDs.
3. Add deterministic titles and read watermarks for new conversations.
4. Keep existing UI layouts while verifying cross-surface parity.

### Phase 3 — product UI

1. Ship Hub Home/Chats/Work/Library navigation.
2. Replace raw-ID rail with History.
3. Ship Mobile Chats/Work destinations.
4. Add TUI `/chats` and `/sessions` compatibility alias.
5. Ship grouped global search and scoped local search.

### Phase 4 — controlled legacy migration

1. Run `backfill-purpose --dry-run` and inspect classifications.
2. Apply bounded purpose batches.
3. Dry-run and apply deterministic legacy title backfill.
4. Remove frontend heuristics only after migration metrics show no unresolved rows.

### Phase 5 — retire mirrors

1. Remove Hub Mobile tab after Channel-backed UI is live.
2. Retain mirror writing for one compatibility release.
3. Remove the `mobile-events.jsonl` writer and deprecated Companion inbox paths.

### Follow-ups (outside this design's acceptance)

- Group-chat creation and `@agent` routing (§7.3).
- Agent GUI / Companion thin-client migration (§6.4, §13).

## 16. Testing

### `mur-common` / `mur-channel`

- Old manifests without purpose deserialize as `None`.
- Explicit purpose always beats inference.
- Legacy Fleet/Workflow/Conversation inference is deterministic.
- Summary reads never mutate manifests.
- Backfill dry-run performs no writes; apply is bounded and idempotent.
- Direct/Group presentation counts only Agent participants.
- Conversation/run filters and ordering follow the shared contract.
- Title generation, fallback, rename, and legacy backfill.
- Read watermark is monotonic and unread excludes internal events.
- Search returns stable grouped metadata and exact event locations.
- Index rebuild reproduces summaries, attention state, and search results.

### Cross-surface regressions

- Hub bound to Conversation A; TUI creates B; Hub's next send remains in A.
- Continuing A makes it latest without creating C.
- Mobile and Hub show the same title, preview, unread count, and active Conversation.
- Search-viewing A does not resume it or clear unread events after the hit.
- Conversation HITL appears in Chats/Home; Fleet HITL appears in Work/Home only.
- Fleet/Workflow channels never appear in Chats.
- Proactive Companion messages appear in the existing Direct conversation.

### UI contracts

- No raw UUID is primary visible text.
- Turn totals are never styled as unread badges.
- Empty Agent rows start a lazy chat without creating an empty Channel.
- Global search groups Chats and Work results.
- `/channels` retains all-purpose advanced access and follow behavior.

## 17. Rejected alternatives

### One inbox containing every Channel

Rejected because storage unification does not imply presentation equivalence. Tool
events, workflow state, and Fleet iterations would overwhelm human conversation.

### Pure agent-first model with Fleet/Workflow hidden elsewhere

Rejected as incomplete. Agent-first is correct inside Chats, but Work still needs a
first-class, consistent surface and global discoverability.

### Flat Direct/Group/Fleet/Workflow/Companion kind

Rejected because it mixes stable purpose, participant-derived presentation, and
message origin. It also requires unnecessary kind changes when participants change.

### Reusing `mur chat` archive search

Rejected because the archive and Channel event store have different ownership,
schemas, lifecycle, and user intent.

### Mutating legacy metadata while listing

Rejected because a read operation must not create hard-to-audit migration writes.
Migration is explicit, dry-runnable, bounded, and idempotent.

## 18. Success criteria

The redesign is complete when:

1. A user can predict that conversations are in Chats and executions are in Work.
2. Hub, Mobile, and TUI show the same conversation title, history, unread state, and binding.
3. No normal UI requires the term Channel or exposes a raw Channel ID as its label.
4. One global search finds both conversation content and work activity with grouped results.
5. Fleet/Workflow events never pollute the Chats inbox.
6. Companion and Mobile no longer maintain independent transcript presentations
   (the Mobile half gates this design; the Companion half lands with the §6.4 follow-up).
7. Legacy migration is observable and reversible through explicit metadata correction,
   without rewriting append-only event history.
# Unified Channel — One Durable Work Object Across Hub, CLI & iOS (v1)

> **Status:** Approved (brainstorm) | **Date:** 2026-06-15
> **Topic:** Collapse ad-hoc agent chat into a single durable `Channel` primitive that is shared
> across every surface, carries a goal that aligns all involved agents over A2A, and runs work in
> two modes (open-ended orchestration + reusable workflow).

## Goal

Today a user talks to MUR agents three ways — the **Hub** GUI, `mur agent cli`, and the **iOS** app —
and every conversation is ad-hoc chat. There is no shared, trackable object behind the chat, so:

1. **Nothing is trackable or monitorable across agents.** Each surface stores chat its own way (or not
   at all), so you cannot see "all work in flight," cannot resume in Hub what you started in the CLI,
   and cannot audit what the agents did.
2. **There is no shared goal.** When several agents collaborate, no single object holds the goal +
   acceptance criteria, so agents drift and there is no A2A mechanism to hand a goal to a peer and get
   a typed result back.
3. **There is no flexible way to "run" work.** "Workflow" today is either a CLI pipeline or a
   `category: Workflow` Skill; neither is attached to a conversation, and there is no place for
   open-ended, LLM-driven orchestration to live next to deterministic, reusable runs.

This spec introduces **one primitive — the `Channel`** — that is the durable spine of work: it owns a
goal, an A2A-standard state, a set of participants (the human owner + agent delegates), and an
append-only event stream. *chat*, *task/issue*, and *channel flow* become three **views** of the same
object. The event stream is the single shared store every surface reads and writes.

The 2026 best-practice research behind this decision converges on exactly this shape (Linear
AgentSession, A2A v0.3 Task lifecycle, Microsoft Agent Framework's split of deterministic workflow vs
open-ended agent orchestration, blackboard/living-spec for goal alignment). See [References](#references).

## Decisions locked (from brainstorming)

| # | Decision | Rationale |
|---|---|---|
| D1 | **One unified primitive** (`Channel`), not separate task/issue/channel types. chat/task/issue are views. | 2026 convergence; least fragmentation; matches user's "one feature not multiple." |
| D2 | **Foundation first**: shared store + the `Channel` object before UI or orchestration. | Everything else depends on it; directly fixes "Hub & CLI see the same chats." |
| D3 | **Hub + CLI in v1; iOS later.** | The two local surfaces share `~/.mur` directly; iOS needs relay sync (separate phase). |
| D4 | **Storage = event-sourced JSONL log + file-watch** (daemon optional, added later as a live-push upgrade). | Matches MUR conventions (session recordings, action-pipeline ledger); offline; no hard daemon dependency. |
| D5 | **`event log` is the single source of truth; SQLite is a rebuildable query index; Postgres/MySQL are server-tier only.** | Local-first; event-sourcing keeps the log canonical and indexes droppable/rebuildable. |
| D6 | **Dual-mode execution on the same Channel**: open-ended LLM orchestration **and** reusable Workflow DAG (`category: Workflow` Skill). | Research: separate deterministic workflow from open-ended agent orchestration, but over one object. |
| D7 | **iOS funnels through the concierge (`mur`)**: home is a Channel list, specialists are *participants* in the one event stream, "direct access" = `@mention` inside the open Channel. | Research + MUR's shipped decisions (voice-mobile P1 single agent; Hub rail spec: user is router, Commander owns orchestration). |
| D8 | **Name = `Channel`.** A2A `Task` keeps its "one run" meaning. | Fits "channel flow" + multi-participant + event stream. |
| D9 | **v1 makes the Channel store canonical** for Hub + CLI; a one-shot importer brings existing `cli-sessions/*.jsonl` in. No prolonged dual-write. | Avoids two sources of truth; the importer preserves back-compat. |
| D10 | **Distinct `ChannelState`, mapped from A2A at the boundary** — do not churn `a2a::TaskState`. | Keeps the A2A type stable; the Channel owns its own lifecycle vocabulary. |

## Scope

- **In (v1 foundation):**
  - A first-class `Channel` type in `mur-common` (goal, state, owner, participants, event records).
  - The on-disk **event-sourced store** (`~/.mur/channels/<id>/`) + a rebuildable **SQLite** query
    index + the existing LanceDB semantic index.
  - **Hub and CLI both read/write the same store**, with file-watch live sync.
  - Migration of the CLI's private `cli-sessions/*.jsonl` and the Hub's in-memory chat onto Channels.
- **Out (later phases / other specs):**
  - The Hub "Work" view, CLI TUI upgrade — **v2** (UI only on top of the v1 store).
  - `channel/delegate` A2A method, agent-attributed events, dual-mode executor, risk-tiered HITL,
    idempotency keys — **v3** (orchestration).
  - iOS relay sync, Channel-list home, `@mention` — **v4**.
  - Server-side persistence (Postgres) and cross-device fleet-sync of Channels — separate spec
    (extends the existing fleet-sync substrate).
  - Cross-network orchestration & governance — remains **Commander's** domain; this spec leaves the
    seam (the supervisor/concierge topology) but builds no cross-host routing.

## Current State (verified)

What already exists and is **reused** (do not rebuild):

| Capability | Location | Reuse |
|---|---|---|
| A2A `Task` + lifecycle (`Submitted/Working/Completed/Failed/Cancelled`) | `mur-common/src/a2a.rs` | A distinct `ChannelState` adopts the A2A v0.3 set and is **mapped at the boundary** (not by churning `a2a::TaskState` — see D10). A2A `Task` stays "one agent run inside a Channel." |
| `Message` / `MessagePart { kind }` (text/data) | `mur-common/src/a2a.rs` | Channel message events wrap A2A messages. |
| `coordination::Plan { goal, steps[] }`, `Step { agent_hint, depends_on, verify_command, judge, phases }`, `FailureCategory`, `RecoveryAction`, `Phase` | `mur-common/src/coordination/{plan,types}.rs` | The DAG-mode execution plan + failure/recovery taxonomy. **Already a "goal + DAG" primitive — not yet wired to A2A RPC.** |
| Workflow = Skill `category: Workflow`, lifecycle, signing, sharing, run-ledger (planned P3 DAG executor) | `mur-common/src/skill/`, `2026-05-28-mur-workflow-engine-design-v2.md` | Dual-mode "reusable workflow" path. |
| Append-only JSONL + atomic temp+rename; rebuildable LanceDB; `mur internals reindex` | `mur-core/src/store/yaml.rs`, `store/vector/`, `session/recordings/` | Storage conventions for the event log + index rebuild. |
| Action-pipeline `TaskQueue` + 7-day JSONL ledger replay | `mur-core/src/action_pipeline/{queue,ledger}.rs` | Pattern for ledger-rebuilt state + idempotent replay. |
| Per-connection streaming (`message/delta`), HITL (`tool/hitl_respond`) | `mur-agent-runtime/src/protocol/methods/`, `task_runner.rs` | Live deltas + HITL surface, reused by Channels. |
| `a2a_dial::{canonicalize_agent_name, dial_method}` (Auto/RequireRunning/ForceEphemeral) | `mur-core/src/a2a_dial.rs` | Transport for the future `channel/delegate`. |
| Cost-router `Router::decide_with_score` | `mur-core/src/route/mod.rs` | Governs specialist spawn cost during orchestration. |
| Harvest proposals + gate | `mur-core/src/harvest/` | Channel → reverse-propose a reusable Workflow. |

What is **missing** (the gaps this spec closes):

- **Chat storage is siloed.** CLI persists to `~/.mur/agents/<agent>/cli-sessions/<id>.jsonl`
  (`mur-core/src/cmd/agent/cli/persist.rs`: `TurnRecord`, `--resume` loads latest). The Hub is
  **in-memory only** (`mur-hub-gui/src-tauri/src/chat.rs`: `agent_chat_send`, `ChatRegistry`; chat is
  lost on close). iOS is **in-memory only** (`mur-mobile-app/Sources/AppModel.swift`:
  `transcript: [ChatLine]`). There is **no shared store**.
- **A planned conversation archive exists but is ingest-oriented**, not live: `~/.mur/conversations/`,
  `mur-common/src/conversation.rs` (`Message { src, conv, role, content }`),
  `mur-core/src/conversations/store.rs`. It records *external* AI sessions (Claude Code, Cursor,
  Slack…) for retrieval — it is **not** a bi-directional, state-machine-bearing live store, so we do
  **not** repurpose it (rejected approach C).
- **No durable object behind chat** — no goal, no state, no participants, no audit object.
- **No agent attribution.** A2A `Message.role` is only `user|agent|system`
  (`mur-common/src/a2a.rs`); when many agents share one stream you cannot tell **which** agent
  posted. This must be fixed before multi-agent participants are legible.
- **No A2A goal-delegation method.** `coordination::Plan` exists but no RPC hands a step/sub-goal to a
  peer and collects a typed Artifact; today an external orchestrator must manually `message/send`.

## Architecture

```
                    ┌──────────────────── one Channel = one goal ────────────────────┐
   Hub  ─ read/write┤  channel.yaml  (goal, state, owner, participants — derivable)   │
   CLI  ─ read/write┤  events.jsonl  (append-only source of truth)                    │
  (iOS ─ later, via ┤    seq | ts | actor(human|agent_id|system) | kind | payload     │
   relay)           │         kind ∈ message|delegation|handoff|tool_*|state_change|  │
                    │              artifact|hitl_request|hitl_response|note           │
                    └───────────────┬───────────────────────────────┬────────────────┘
                       file-watch (live)                    rebuildable indexes
                    (notify crate; daemon push           ┌─ SQLite  channels.db (query)
                     is an optional upgrade)             └─ LanceDB index.lance (semantic)

   Execution over a Channel (v3):
     mode 1 open-ended : concierge(mur) LLM decomposes → channel/delegate → specialists
                         post attributed events back into the SAME stream
     mode 2 workflow   : run a category:Workflow Skill (coordination::Plan DAG)
     both gated by risk-tiered, hash-pinned HITL; external writes carry idempotency keys
```

### 1. The `Channel` data model (`mur-common`)

```rust
struct Channel {
    id: String,                 // uuid v7 (time-sortable, like cli-sessions today)
    title: String,
    goal: Goal,                 // living spec, kept OUT of the prompt
    state: ChannelState,        // A2A v0.3 lifecycle
    owner: Actor,               // always the human — delegation never transfers ownership
    participants: Vec<Participant>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    // event stream is stored separately (events.jsonl), not inlined
}

struct Goal { statement: String, acceptance_criteria: Vec<String> }

enum ChannelState {            // adopt A2A v0.3 verbatim
    Submitted, Working, InputRequired, Completed, Failed, Canceled, Rejected, Stale,
}

struct Participant { actor: Actor, role: ParticipantRole, joined_at: DateTime<Utc> }
enum Actor { Human { name: String }, Agent { id: String }, System }
enum ParticipantRole { Owner, Router, Delegate, Observer }

struct ChannelEvent {
    seq: u64,                   // monotonic per channel
    ts: DateTime<Utc>,
    actor: Actor,              // WHO — closes the agent-attribution gap
    kind: EventKind,
    payload: serde_json::Value,
    idempotency_key: Option<String>,   // hash(channel_id, step_id) for external-write events
}

enum EventKind {
    Message,        // a chat / A2A message (wraps a2a::Message)
    Delegation,     // concierge handed a sub-goal to a participant
    Handoff,        // control transfer between agents
    ToolCall, ToolResult,
    StateChange,    // ChannelState transition
    Artifact,       // typed output (A2A Artifact)
    HitlRequest, HitlResponse,
    Note,
}
```

`chat` = the `Message`/`Note` events; `task/issue` = `goal` + `state` + the run events; `channel
flow` = the event stream itself. One type, three views.

### 2. Storage & indexes

- **Source of truth:** `~/.mur/channels/<id>/events.jsonl` — append-only, atomic append, monotonic
  `seq`. Mirrors `session/recordings/*.jsonl` conventions.
- **Manifest:** `~/.mur/channels/<id>/channel.yaml` — a *cache* of goal/state/participants written
  via temp+rename; fully recomputable by folding the event log.
- **SQLite read-model:** `~/.mur/index/channels.db` — drives list/filter/sort/paginate and the
  cross-agent "my work" inbox. Embedded, single-file, no server, rebuildable from the logs.
- **LanceDB:** semantic search over event text (existing infra).
- `mur internals reindex` rebuilds **both** indexes from the logs; deleting either is safe.

### 3. Cross-surface sharing & live sync (the v1 payoff)

- Hub and CLI open the **same** `events.jsonl` for a Channel; appends from one are seen by the other.
- **Live:** an OS file-watch (`notify`) tails the log and pushes new events to the UI; a single
  append lock keeps `seq` monotonic and avoids interleaving.
- **Daemon (optional upgrade, not v1):** when the runtime/daemon is running it can broadcast deltas
  over its existing per-connection socket; file-watch is the always-available fallback. No hard daemon
  dependency.
- **Migration:** CLI `cli-sessions/*.jsonl` and Hub in-memory chat both move onto Channels. CLI
  `--resume` resolves to "open the latest Channel"; the Hub chat survives app restart.

### 4. Goal alignment & A2A delegation (v3)

- **Delegation, not assignment:** the human stays `Owner`; the concierge (`mur`) is `Router`;
  specialists are `Delegate` participants. The Channel always appears in the owner's "my work" view.
- **Goal alignment:** `Goal` is an external living spec; on delegation the concierge passes
  `goal + acceptance_criteria` to the peer, countering context drift/poisoning (the dominant
  multi-agent failure mode).
- **New A2A methods** (added to `mur-agent-runtime` dispatcher alongside `message/send`):
  - `channel/delegate` — hand a sub-goal to a participant agent; the peer's run posts
    **attributed** events back into the Channel and returns a typed `Artifact`.
  - `channel/subscribe` — a surface or agent subscribes to a Channel's event stream.
- **Topology:** central **supervisor (concierge), one level deep** — no peer-to-peer choreography
  (would create a second orchestration path that bypasses Commander governance; cf. Hub rail spec).
- **Identity:** plan migration to A2A v1.0 **signed Agent Cards** as the spoof-proof upgrade from the
  current exact-match name check (`canonicalize_agent_name`).

### 5. Execution — dual mode + HITL + durability (v3)

- **Mode 1 — open-ended:** concierge LLM decomposes the goal and `channel/delegate`s to specialists
  (selective fan-out: trivial turns do not fan out; cost-router governs spawn).
- **Mode 2 — reusable workflow:** run a `category: Workflow` Skill, i.e. the `coordination::Plan`
  step DAG (deterministic, auditable) via the workflow-engine-v2 executor.
- Both modes write into the **same** Channel event stream. Harvest can reverse-propose a Workflow
  from a completed Channel (existing harvest path).
- **HITL:** **hash-pinned** (pin the action args by hash at interrupt; refuse on drift — the #1
  production HITL bug), **risk-tiered** (gate only high-risk: cost-router spawn, deletion, entitlement/
  secret change, outbound messaging — reuse existing entitlements), answerable from any surface,
  recorded as `HitlRequest`/`HitlResponse` events for audit.
- **Durable execution:** every external-write tool/A2A call carries
  `idempotency_key = hash(channel_id, step_id)`, so crash-replay never double-applies side effects.

### 6. UI/UX

- **Hub (v2):** add an "Agents | Work" toggle. The **Work** view = left rail (Channel list + state
  badges) / center (single event stream with per-agent attribution + collapsible specialist summary
  cards + inline HITL cards) / right (participants + trace/plan progress). Fits the 1024×768 target by
  reusing the existing detail-panel + conversation-rail layout slots
  (`mur-hub-gui/ui/src/components/{ConversationRail,ChatTab}.tsx`).
- **CLI (v2):** `mur agent cli` opens a Channel; `murmur a1 a2 a3` multiplex panes show each agent's
  in-Channel lifecycle state, not raw streams.
- **iOS (v4):** Channel-list home + concierge funnel + `@mention` (depends on agent attribution from
  §1/§4). No agent roster home screen.

## Phasing

| Phase | Contents | Surfaces |
|---|---|---|
| **v1 — foundation (this spec's core)** | `Channel` type; `~/.mur/channels/` event store; SQLite + LanceDB indexes; Hub & CLI read/write same store; file-watch live sync; migrate existing chat. | Hub, CLI |
| **v2 — UI** | Hub "Work" view (list/feed/trace); CLI TUI upgrade. | Hub, CLI |
| **v3 — orchestration** | `channel/delegate` + `channel/subscribe`; agent-attributed events; dual-mode executor; risk-tiered hash-pinned HITL; idempotency keys. | Hub, CLI |
| **v4 — iOS** | Relay-sync Channels to the app; Channel-list home; concierge funnel; `@mention`. | iOS |
| later | Server (Postgres) persistence + cross-device Channel fleet-sync. | — |

## Error handling

- Failure classification & recovery reuse `coordination::{FailureCategory, RecoveryAction}`
  (Retry / Reroute / Escalate / Abort).
- Concurrent append: single writer lock + monotonic `seq`; consumers de-dup via `idempotency_key`.
- Index integrity: SQLite/LanceDB are derived; on corruption or version skew, drop and rebuild from
  the logs (`mur internals reindex`).
- Daemon-absent path always works (file-watch); daemon push is best-effort.

## Testing

- `Channel`/`ChannelEvent` serde round-trip; A2A state mapping.
- Event-log append → fold → manifest equals written manifest (event-sourcing invariant).
- **Cross-surface integration:** CLI appends an event; Hub (file-watch) observes it; and vice versa.
- Index rebuild: delete SQLite/LanceDB → rebuild → queries match a log-fold oracle.
- HITL: hash-pin drift is refused; idempotency-key replay applies a side effect exactly once.
- Migration: an existing `cli-sessions/*.jsonl` imports into a Channel with order preserved; Hub chat
  survives a simulated restart.

## Resolved during review

The three questions raised at the brainstorm review are locked — see **D8** (name = `Channel`),
**D9** (the v1 store is canonical + a one-shot importer, no prolonged dual-write), and **D10**
(distinct `ChannelState` mapped from A2A at the boundary). No open questions remain for v1; later
phases (v3 delegation, v4 iOS) will be detailed in their own plans.

## References

Internal specs reused / bordered by this design:

- `docs/superpowers/specs/2026-05-28-mur-workflow-engine-design-v2.md` — Workflow = Skill; DAG executor.
- `docs/superpowers/specs/2026-06-08-hub-multiagent-conversation-rail-design.md` — Hub rail; "user is
  the router, Commander owns orchestration" boundary.
- `docs/superpowers/specs/2026-06-11-mur-ambient-capture-and-harvest-design.md` — event-log + harvest.
- `docs/superpowers/specs/2026-05-31-agent-action-pipeline-design.md` — ledger replay / archive-don't-delete.
- `docs/superpowers/specs/2026-06-01-cost-router-orchestrator-design.md` — governed specialist spawn.
- `docs/superpowers/specs/2026-04-19-mur-conversations-design.md` — the ingest archive (explicitly NOT
  reused as the live store).

External research (2026 best practices) — full digests captured during brainstorming:

- A2A Protocol v0.3/v1.0 task state machine + Artifacts — https://a2a-protocol.org/v0.3.0/specification/
- Linear Agents (AgentSession, delegation-not-assignment) — https://linear.app/developers/agents
- Microsoft Agent Framework (split deterministic workflow vs open-ended agent orchestration).
- Durable execution & idempotency — https://docs.langchain.com/oss/python/langgraph/durable-execution ,
  https://www.diagrid.io/blog/checkpoints-are-not-durable-execution-why-langgraph-crewai-google-adk-and-others-fall-short-for-production-agent-workflows
- HITL hash-pinning / risk-tiering — https://growwstacks.com/blog/human-in-the-loop-ai-agents-langgraph
- Context engineering / poisoned-context — https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
- Mobile single-funnel orchestration (Siri system-orchestrator, ChatGPT Agent Mode, agent-as-channel-member).

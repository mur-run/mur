# Unified Channel v3 — Orchestration Design

> **Status:** Design (brainstorm) — awaiting review
> **Date:** 2026-06-16
> **Topic:** Execution over a Channel — dual-mode executor, agent-attributed delegation, risk-tiered hash-pinned HITL, idempotency. Builds on v1 (store, SHIPPED) and v2 (Work view + CLI, planned `2026-06-15-unified-channel-v2-ui.md`).
> **Decomposition:** this is an umbrella spec for four sub-projects (v3a–v3d). Only **v3a** is detailed enough to plan immediately; v3b/v3c get their own specs as they come up.

---

## 1. Goal

Make a `Channel` *executable*: a goal on a channel can be carried out by running work and recording every step as attributed, durable events in the same event log that v1 persists and v2 renders. Two execution modes share one channel and one event model:

- **Mode 2 — reusable Workflow:** run a `category: Workflow` Skill's `coordination::Plan` step DAG over the channel (deterministic, reuses the SHIPPED DAG executor). **Ships first.**
- **Mode 1 — open-ended:** the concierge LLM decomposes the goal and delegates sub-goals to specialist agents; their work is attributed back into the same channel.

Both are gated by risk-tiered, hash-pinned human-in-the-loop (HITL), and made crash-safe by idempotency keys.

This spec resolves the two design decisions the user flagged for deep analysis (delegate attribution; HITL model), locks the trust model, and decomposes v3 into independently-shippable sub-projects. The decisions were deliberated by an independent multi-agent panel (steelman + adversarial), grounded in the shipped code; see §9 for the rejected alternatives.

## 2. Substrate (already shipped — reused, not rebuilt)

- **Channel store (v1):** `~/.mur/channels/<id>/{events.jsonl, channel.yaml}`; `mur-channel::ChannelService::{open, create_for_agent, append_message(channel_id, actor, kind, text, task_id), load_events, list, latest_for_agent}`; `append_event` serializes concurrent cross-process writes with monotonic `seq` via the `events.lock` sidecar. `EventKind` already includes `Delegation, Handoff, ToolCall, ToolResult, StateChange, Artifact, HitlRequest, HitlResponse`. `ChannelEvent` already has `idempotency_key: Option<String>` (**defined but unused**).
- **DAG executor (workflow-engine v2, P3):** `mur-core/src/executor/dag.rs::execute_dag()` over `coordination::Plan` / `ProcedureStep` DAGs — topo-sort, concurrent ranks, per-step `needs_approval`, `on_failure {Skip,Abort,Retry}` + retry/timeout, run-ledger. A Workflow is a `Skill` with `category: Workflow`.
- **A2A runtime:** dispatcher with `message/send` (+ streaming `message/delta`), `tasks/cancel`, `tool/hitl_respond`, `agent/card`. `task_runner.rs` runs the agentic loop; HITL today is **post-execution**.
- **Cross-agent dial:** `mur-core/src/a2a_dial.rs::{dial_method, dial_message_streaming}` with `DialMode {Auto, RequireRunning, ForceEphemeral}`; `canonicalize_agent_name` (case-insensitive); the target's identity is self-verified at startup (`verify_name_match`, exact argv0==profile.name).
- **Identity:** `mur-common::AgentIdentity::{sign_bytes, verifying_key}`, key rotation via `verify_chain` (`identity.rs`). Used by v3d.
- **Cost router (Phase 1):** `Router` + `EscalationLedger` (local/frontier routing). Phase 2 governed vendor spawn is **deferred** (gated on P0b) — out of v3 scope.

## 3. Boundaries (LOCKED — do not violate in any sub-project)

1. **Per-user, one level deep, no cross-host.** v3 is a single concierge orchestrating local specialists. Cross-network routing/governance belongs to **Commander**; v3 leaves that seam but builds none of it.
2. **Delegation targets existing MUR agents** via A2A dial — **not** spawning vendor CLIs (claude/codex). Governed vendor spawn is cost-router Phase 2, deferred.
3. **No regression vs v1's attribution/trust.** Any new write path must be no weaker than the shipped v1 channel write path.
4. **Read order is `seq`, never `ts`.** `ts` is per-process wall-clock and can move backwards across writers; all folders (reindex, Hub Work view, CLI) order by `seq` only.

## 4. Decision A — delegate attribution: **A1 (concierge-mediated, single trusted writer)**

**Chosen:** the concierge is the **sole writer** of channel events for a delegated turn. It dials the specialist via the existing `dial_message_streaming`/`dial_method` (`DialMode::RequireRunning`), and stamps the events itself: a `Delegation` event (actor `System` or `Agent{mur}`) then the specialist's reply as `Message` with actor `Agent{<canonical specialist name>}`. **The specialist stays a vanilla A2A agent that never learns the word "channel."**

**Why (grounded):**
- A1 *is* the shipped v1 write path — `cli/persist.rs::append()` already opens `ChannelService` and the orchestrating process stamps `Agent{id}` for the reply and `local_human()` for the user. `append_message` taking an explicit `actor` was designed for exactly this caller-chooses-actor pattern.
- A2 (peer-writes-own) requires genuinely new surface: `mur-agent-runtime` has **zero** `mur-channel` dependency today, so A2 means a new crate dep + a `channel/delegate` dispatcher method + re-shipping every specialist runtime — to deliver step-level attribution the one-level-deep v3 unit of work does not need.
- The trust model today authenticates *startup, not writes*: `verify_name_match` is a process self-check, the unix-socket transport has no peer credential, and `append_event` writes whatever actor it is handed. **Unsigned A2 lets any local process forge `actor=Agent{other}` or even `Human{name}` (forging a human approval)** — a strict security regression. A1 collapses the write surface to one trusted writer per channel, and composes with the dial guarantees (`canonicalize_agent_name` + the target's `running.lock` socket) so the stamped actor is provably the agent dialed — **no per-event crypto needed in the single-host v3 trust domain.**
- A1 keeps the log linearizable (one writer per delegated turn; the v4 iOS "sync since seq N" relay depends on this) and has clean failure semantics (dial returns `Result`; on failure nothing is written — no phantom/half-attributed reply; DAG `on_failure` drives retry).

**Design:**
- **Schema reservation (do in v3a, zero behavior change):** add to `mur-common/src/channel.rs` `ChannelEvent` two back-compatible fields — `#[serde(default, skip_serializing_if = "Option::is_none")] sig: Option<String>` and `... key_version: Option<u32>`. No `CHANNEL_SCHEMA_VERSION` bump (the file's own rule: optional `#[serde(default)]` fields don't bump). They stay `None` until v3d signing. **Reserve the canonical sign-input in a doc-comment now:** sign over `{channel_id, actor, kind, payload, idempotency_key}` **excluding** store-assigned `seq` and `ts`. This one agreement prevents a future schema bump.
- **Delegation payload (typed):** `{ target_agent: <canonical name from canonicalize_agent_name, NOT the user-typed string>, child_task_id, parent_channel_id }`. Add `ChannelService::append_delegation(channel_id, target_agent, child_task_id)`. Every downstream event for that turn carries the same `child_task_id` in `payload.task_id`.
- **Write flow (concierge-side, all in mur-core), per delegated step:** (a) append `Delegation` event; (b) `dial_message_streaming(home, target, params, on_delta, on_hitl)` with `DialMode::RequireRunning` (never `Auto` — Auto can cold-spawn a different process and corrupt attribution); (c) on success append the reply as `Agent{<canonical target>}`/`Message`, same `child_task_id`, **and store the `tasks/get` snapshot alongside** so attribution is auditable, not merely asserted; (d) on dial failure append a `Note`/`StateChange` and let DAG `on_failure` retry.
- **Anti-forgery stance (v3a–v3c):** trust = single trusted writer + caller-stamped actor + socket-addressed dial to a `running.lock`-published peer that self-verified its name. **No per-event signatures yet.** Residual exposure (documented, accepted under the locked single-host boundary): any local process that learns a channel id can append forged events — fine for a single-user `~/.mur`, disqualifying if v3 ever spans hosts. The A1→A2 evolution is gated entirely on v3d signing.

**Files (v3a's share):** `channel.rs` (two optional fields + doc-comment), `mur-channel/src/service.rs` (`append_delegation` + typed payload). The delegation *use* of this is v3b. **Zero** runtime/dispatcher/specialist changes.

## 5. Decision B — HITL: **B1-by-tier, durable-Channel approval substrate**

**Chosen:** a risk-tiered, hash-pinned HITL where **gate placement is decided by tier**, and **both** execution modes converge on **one shared helper** that writes durable Channel `HitlRequest`/`HitlResponse` events.

**Rules:**
1. **Tier drives placement.** Pre-execution gate for `Write/Destructive/Spend/Privileged/NetworkEgress`; post-execution **audit-echo (non-blocking)** only for `Read`/idempotent-query. Today's post-exec gate (`task_runner.rs` runs the tool at ~785 *before* emitting `tool/approval_needed`) is a result-suppression control, not a safety gate — correct for the read tier only, unsafe theater for mutating tiers.
2. **Tier is resolved most-restrictive-wins:** `max(tool-declared base tier, channel-policy floor, entitlement clamp)`. **Never** LLM-asserted, never read from `tool_input` or observed content. The existing `ToolPolicy{Allow,Ask,Deny}` is an orthogonal entitlement *ceiling* (can tighten, not loosen).
3. **Hash-pin = SHA-256** (the `sha2` crate is already a dep in `mur-common`, `mur-agent-runtime`, `mur-core`) over canonical JSON of `{tool_name, normalized_input, channel_id, step_or_call_id, agent_id}`, computed at gate time, embedded in `HitlRequest`, echoed in `HitlResponse`, **re-computed at the execute boundary, fail-closed on mismatch**. Pin the **post-substitution** args (after `{{var}}`/`{{input}}` injection) so pinned bytes == executed bytes. **Do not** reuse `task_runner.rs::fingerprint_args` — it uses `std::DefaultHasher` (build-local seed, scoped to "equality within a window"), unfit for a durable cross-process pin.
4. **Authoritative request/response = the Channel `HitlRequest`/`HitlResponse` event pair** under the `events.lock` sidecar; `tool/approval_needed` + `tool/hitl_respond` remain a low-latency **advisory mirror** that reconciles against the log, never a second source of truth. **Until v3d signing, the channel `HitlResponse` is a durable mirror, not authority** — a local process can forge one. The agent's authenticated unix-socket `tool/hitl_respond` remains the actual grant.
5. **`idempotency_key` becomes load-bearing and deterministic:** `sha256(channel_id|run_id|step_id|attempt)` for DAG steps, `sha256(channel_id|task_id|call_id)` for tool calls. `append_event` becomes **dedup-aware** so a crash-rerun neither re-prompts nor re-applies.

**Design types (mur-common):** `enum RiskTier { Read, Write, Destructive, Spend, Privileged, NetworkEgress }` (Ord by severity); `enum HitlMode { Auto, Ask, Deny }`; additive `#[serde(default)] risk: Option<RiskTier>` on `ToolRule` **and** a new per-step `RiskTier` on `ProcedureStep` (⚠ **both are NEW additive fields — `ProcedureStep` today has `needs_approval: bool` but NO tier**). Channel payloads: `HitlRequest{hitl_id, action_hash, tier, tool_name, tool_input, task_or_step_id, agent_id, timeout_ms, summary}`, `HitlResponse{hitl_id, action_hash, allow, reason, decided_by: ChannelActor, surface}`.

**Shared gate flow** (`hitl::gate(ActionRequest) -> Decision`, used by mode 1, mode 2, Hub/CLI/iOS): compute `action_hash` + deterministic `idempotency_key`; resolve effective tier; `Auto` → proceed (read tier emits non-blocking audit `ToolCall`/`ToolResult`); `Ask` → set `ChannelState::InputRequired`, append `HitlRequest`, also emit the advisory `tool/approval_needed`, block tailing `events.jsonl` for a matching `HitlResponse`, on wake **re-verify hash (fail-closed)**, execute, append `ToolCall`+`ToolResult`; `Deny` → refuse pre-emptively.

**Files:** `channel.rs` (payload structs; bump `CHANNEL_SCHEMA_VERSION` → 2 **only here**, once a waiter blocks on `HitlResponse` — a reader silently skipping it could re-apply an effect); `agent.rs` (`RiskTier`, `ToolRule.risk`, tier→`HitlMode` table); `skill/manifest.rs` + coordination (per-step tier); **new** `mur-core/src/hitl/gate.rs`; `mur-channel/src/store.rs` `append_event` → dedup-aware on `idempotency_key`; `mur-core/src/executor/dag.rs` `execute_step` — replace the `needs_approval` dialoguer/**non-TTY-silent-skip** branch with the channel-gated `hitl::gate` await (the silent-skip currently reports `success=true` — a fail-closed landmine); `task_runner.rs` `handle_tool_call` reorder for mutating tiers (deferred to mode-1 hardening, see §6); command-mode `sh -c` steps gate at the **step tier** (their args bypass tool classification).

## 6. Trust-model invariant (stated once, held across v3a–v3c)

> The channel log is a **trusted-single-writer** log (the concierge). The channel `HitlResponse` is a **durable mirror**; the agent's authenticated unix-socket `tool/hitl_respond` is the **actual HITL grant**. "Channel-event-as-authority" (headless Hub/iOS approval) **and** A2 peer-writes-own attribution are **both** unlocked by the **single v3d Ed25519 signing project** — neither lands piecemeal before signing.

## 7. Decomposition (dependency order: v3a → v3b → v3c; v3d cross-cutting, deferred)

### v3a — Workflow-DAG mode 2 over a Channel *(ships first; gets its own implementation plan now)*
**Scope:** run the **existing** DAG executor's local **command/intent** steps over a channel, emitting an attributed event trail — no delegation, no HITL hardening. Thread `ChannelService` + `channel_id` + `run_id` into `DagExecOptions` and through the per-rank `tokio::spawn` seam (`dag.rs` ~388-405 carries **no** channel handle today); each DAG step emits an attributed channel event (`ToolCall`/`ToolResult`/`Artifact`/`Note`, actor `System`/`Agent{orchestrator}` — the run process is sole writer); `StateChange` events for channel lifecycle (`Working`→`Completed`/`Failed`); **reserve the schema** (`ChannelEvent.sig`/`key_version` + canonical sign-input doc-comment, both `None`).
**`needs_approval` stance (fail-closed, no landmine):** v3a runs **approval-free** workflows. A workflow containing a `needs_approval` step is **refused** up front with a clear "requires HITL (v3c)" message — v3a must **not** inherit the executor's non-TTY silent-skip-as-success (`dag.rs` ~195-206). The real interactive channel-gated approval lands in v3c.
**Consumes:** Decision A (sole-writer write-flow + the schema-reservation half). **No runtime changes, no new dispatcher method, no dial.** This is the shared executor-plumbing prerequisite both later increments extend.

### v3b — Delegation / mode 1
**Scope:** per delegated step, `append_delegation` (typed payload, canonical target name); dial via `dial_message_streaming(DialMode::RequireRunning)`; on success append reply as `Agent{canonical target}` + store the `tasks/get` snapshot; forward the specialist's `on_hitl` up and relay the human's `tool/hitl_respond` down. **Sets a deterministic `idempotency_key`** on the Delegation+reply appends even before dedup is enforced. **Consumes:** Decision A. **Depends on** v3a (executor seam plumbed). Zero runtime/dispatcher/specialist changes.

### v3c — HITL hardening
**Scope:** `RiskTier`/`HitlMode`/`ToolRule.risk` + per-step `RiskTier` on `ProcedureStep` (additive); shared `hitl::gate` with canonical SHA-256 pin + deterministic `idempotency_key`; **replace** the `dag.rs` `needs_approval` silent-skip with a channel-gated await (the one non-optional executor change); write `HitlRequest`/`HitlResponse` events; **dedup-aware `append_event`** (the shared store change — **owned here**, consumed-but-not-duplicated by v3b); fold-to-completed-effects **resume cursor** (the DAG has none — restarts at rank 0); `ChannelState::InputRequired` while outstanding; gate command-mode `sh -c` steps at step tier. **Consumes:** Decision B. **Depends on** v3a; reconciles with v3b's keys. **`CHANNEL_SCHEMA_VERSION` → 2 here.**

### v3d — Per-event Ed25519 signing *(DEFERRED, cross-cutting)*
**Scope:** sign-on-append via `AgentIdentity::sign_bytes` over the reserved sign-input; verify-on-fold against `identity.pub` resolved by `key_version` through `verify_chain` (so `mur agent rekey` doesn't invalidate history); drop malformed/forged/unsigned `Agent` lines at fold with a logged anomaly. **Unlocks simultaneously:** Decision A's A1→A2 (peer-writes-own) **and** Decision B's authority-bearing channel `HitlResponse`. Also folds in `idempotency_key` anti-replay, the runtime `channel/delegate` dispatcher method + `mur-channel` dep in the runtime, the `task_runner.rs` `handle_tool_call` reorder (gate mutating tiers *before* `tool.execute()`), and the cached-tail `next_seq` optimization. **One project, two consumers** — do not double-count.

## 8. Shared seams & sequencing rules (must hold across sub-projects)

1. **Executor channel-plumbing is a v3a deliverable, not a by-product.** The per-rank spawn clones owned opts with no channel handle; both v3b (delegation appends) and v3c (`hitl::gate`) need it. Plumb once in v3a or they collide.
2. **Dedup-aware `append_event` + deterministic `idempotency_key` is a single change owned by v3c.** v3b merely **sets** the key (no dedup logic). `append_event` ignores `idempotency_key` today.
3. **Schema reservation in v3a; schema bump in v3c.** The `sig`/`key_version` optional fields ship `None` in v3a (no bump). `CHANNEL_SCHEMA_VERSION` → 2 happens only in v3c, when a blocking waiter depends on `HitlResponse`.
4. **`ProcedureStep` per-step `RiskTier` and `ToolRule.risk` are NEW fields** (the deliberation's one factual correction — they do not exist today; only `needs_approval: bool` does).
5. **Scope-fence v3a:** v3a runs the **existing local** command/intent steps over a channel and only *emits attributed events* — it does **not** dial specialists (that is v3b) and does **not** harden HITL. Command-mode `sh -c` tier-gating and the `needs_approval`→`hitl::gate` replacement (including closing the non-TTY silent-skip-as-success) belong in **v3c**; v3a sidesteps the landmine by refusing approval-bearing workflows rather than running them.

## 9. Rejected alternatives (decisive)

- **Unsigned A2 (peer-writes-own without signatures)** — strict security regression: shared `~/.mur` + actor-blind `append_event` lets any local process forge `Agent{other}`/`Human{name}`. Buys nothing over A1 except write contention.
- **Signed A2 as the *first* increment** — correct destination, wrong increment: needs runtime `mur-channel` dep + new dispatcher method + N specialist re-ships + sign/verify + `key_version` rotation, all to deliver step-level attribution the one-level-deep unit doesn't need. Deferred to v3d.
- **`DialMode::Auto` for delegation** — can cold-spawn an unintended ephemeral process and corrupt the stamped actor. Use `RequireRunning` + record the **canonical resolved** name.
- **B2 (keep post-exec gate) as the general HITL model** — cannot gate irreversible side effects (the effect already ran before the human sees the prompt). Kept only for the read tier.
- **Reusing `fingerprint_args` as the pin** — `DefaultHasher`, build-local seed; unfit for a durable cross-process pin. SHA-256 via `sha2` is mandatory.
- **Channel `HitlResponse` as standalone authority (pre-signing)** — forgeable by any `~/.mur`-sharing process. Authority stays on the socket until v3d.
- **Leaving the DAG non-TTY branch as silent-skip-as-success** — makes a headless high-risk step a silent no-op reported as success; the opposite of fail-closed.
- **Random/per-call `idempotency_key`** — defeats crash-rerun dedup; must be deterministic.

## 10. Open risks (carried into the relevant sub-project plans)

- **Pin canonicalization stability:** `serde_json` key-order is stable (no `preserve_order`), but float formatting, unicode (need NFC), and DAG variable substitution can still drift gate-time vs execute-time bytes → false `hitl_drift` self-DoS. Pin over **post-substitution** args; pin a canonicalization version; test heavily at both call sites. *(v3c)*
- **Concierge as single point of write-failure/trust (A1):** crash after the specialist replied but before `append_message` loses the work and a retry can double-write. Mitigation: set the deterministic `idempotency_key` now (v3b) even though dedup enforcement is v3c; store the `tasks/get` snapshot. *(v3b/v3c)*
- **HITL latency across two hops (delegation):** the specialist's `tool/approval_needed` must reach the human and return before the specialist's 300s auto-deny fires. Needs explicit end-to-end timeout handling, not the per-hop default. *(v3b)*
- **Channel-policy floor is a new authority surface** — "auto-approve all" via prompt-injection re-introduces bypass. Channel-policy mutation and entitlement loosening must themselves be `Privileged`-tier HITL actions. *(v3c)*
- **Concurrent ranks:** multiple mutating steps in one rank each pre-gate; Hub/CLI must route the response by `action_hash`, not "latest pending." *(v3c)*
- **Resume-cursor cost:** folding the full log on every restart is O(n) per channel; long-lived channels need a compacted snapshot/cursor in `channel.yaml` (rebuildable). *(v3c)*
- **Read-with-side-effects defeats a-priori tiering** (GET-to-webhook, mutating "lookup" MCP). Mitigation is fail-closed (unknown/network-capable ⇒ high tier); partial, own it. *(v3c)*
- **Effect-idempotency ≠ channel-idempotency:** channel dedup prevents re-prompt/re-ledger, but OS-level effect idempotency (e.g. `git push`) is the tool's job; `on_failure=Retry` should default high-risk retries to re-Ask. *(v3c)*
- **`task_id` uniqueness** is unenforced; the concierge must own the `{target_agent, child_task_id, parent_channel_id}` convention (the store won't). *(v3b)*

## 11. References

- `docs/superpowers/specs/2026-06-15-unified-channel-design.md` — v1 design (D1–D10), §4–§5 v3 sketch this spec resolves.
- `docs/superpowers/plans/2026-06-15-unified-channel-v2-ui.md` — v2 Work view + CLI (the surfaces that render these events).
- `docs/superpowers/specs/2026-05-28-mur-workflow-engine-design-v2.md` — Workflow = Skill; DAG executor (mode 2 substrate).
- `docs/superpowers/specs/2026-06-01-cost-router-orchestrator-design.md` — governed spawn (Phase 2, deferred; out of v3 scope).
- A2A v0.3 task/message state machine; HITL hash-pinning ("approve A, execute B" drift bug); risk-tiering to avoid approval fatigue.

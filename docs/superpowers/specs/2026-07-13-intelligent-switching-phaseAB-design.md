# Intelligent Switching — Phase A+B (Smart Background Routing + Outcome Recording) Design Spec

> **Date**: 2026-07-13
> **Status**: Ready for review
> **Scope**: Phase A — automatic cheap-model routing for background turns with cascade escalate-on-failure and per-decision transparency ("Smart mode"). Phase B — local routing-outcome recording that feeds the future memory router. Builds on model-switching Phase 1 (#692, `FallbackLlmClient`/`resolve_model_refs`) and Phase 2 (#693, Hub GUI).
> **Out of scope**: interactive-turn downgrading (per-task or per-hop), learned/kNN routing (Phase C), fleet-member routing (members carry explicit `model_ref`s).

## Overview

Most users cannot judge when to switch models, so MUR switches for them — but only where the signal is strong enough to justify automation:

> **Automation appetite must match signal strength.** "This is a background turn" is a certain fact with a small blast radius (nobody is watching; results are reviewable) → automate now. "This interactive task is easy" is a weak guess with a large blast radius (user-facing quality) → do NOT automate until the Phase-C memory signal earns it.

Phase A therefore downgrades **background turns only**: scheduled tasks, companion outbox generation, and other runtime-initiated calls run on a cheap model first, escalating to the agent's normal model on structural failure (cascade). Every decision is visible (Hub chat shows model + reason) and correctable (one-click re-run on the smart model). Interactive turns keep today's behavior untouched.

Phase B records the outcome of **every** routed turn — including interactive ones — into local telemetry. These records are the training data for Phase C's kNN memory router ("tasks like this succeeded on haiku 96% of the time"), so recording starts now even though the learned router comes later. Every week without recording is lost training data.

### Design principles

1. **Fail-expensive.** When unsure, use the better model. Wrong downgrades destroy trust; wrong upgrades only cost money.
2. **Transparency is mandatory.** No silent switching: every smart decision is displayed and logged.
3. **Explicit beats smart.** A user- or profile-pinned model is never overridden (fleet members, `--model`, per-agent `model_ref` on interactive turns).
4. **Do not touch Phase-1 safety boundaries.** The retryable/fatal classification for interactive traffic is unchanged.

## Architecture (Approach 3 — intent tag + centralized decision)

```
call sites (know the context)          single choke point (decides/records)
────────────────────────────          ───────────────────────────────────────
scheduled task runner   ─┐
companion outbox gen    ─┼─ LlmRequest{ intent } ──▶ FallbackLlmClient
sleep-cycle generates   ─┘                            ├─ candidates_for(req):
chat / A2A / delegate  ── (default Interactive)       │    Background+Smart → [cheap, …chain, primary]
                                                      │    Interactive     → Phase-1 behavior (unchanged)
                                                      ├─ generate loop: retry → cascade escalate → done
                                                      └─ record → TelemetryWriter (mur.routing JSONL)
```

- Context is tagged where it is known (one line per call site); decision, cascade, and recording live in the existing Phase-1/2 choke point.
- `LlmRequest` gains one defaulted field — old code compiles and behaves identically.

## Components

### 1. `RequestIntent` (`mur-agent-runtime/src/llm/mod.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestIntent {
    /// User-facing turns: chat, A2A message/send, fleet channel/delegate.
    #[default]
    Interactive,
    /// Runtime-initiated turns nobody is watching live.
    Background(BackgroundKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    Scheduled,   // `mur agent schedule` task executions
    Companion,   // companion outbox message generation
    Maintenance, // other runtime-initiated generates (sleep-cycle adjacent)
}
```

`LlmRequest` gains `#[serde-irrelevant] pub intent: RequestIntent` (the struct is not serialized across processes; plain field with `Default`). Call sites that know they are background set it explicitly; everything else stays `Interactive` by default. **Fleet delegate turns remain `Interactive`** — members carry explicit `model_ref`s and explicit beats smart.

### 2. Smart config (`mur-common/src/config.rs`, extends `ModelSwitchConfig`)

```yaml
models:
  smart:
    enabled: true          # DEFAULT ON — automation is MUR's default posture
    cheap: null            # model_ref; null = auto-pick (see below)
    max_escalations: 1     # per task; cascade cap (const DEFAULT_SMART_MAX_ESCALATIONS)
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmartConfig {
    pub enabled: bool,              // serde default = true (default_true())
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,      // validated against models.yaml when set
    pub max_escalations: u32,       // serde default = DEFAULT_SMART_MAX_ESCALATIONS (1)
}
```

- `ModelSwitchConfig` gains `#[serde(default)] pub smart: SmartConfig`. Legacy configs deserialize to Smart-on with auto-pick.
- **Auto-pick rule** (when `cheap` is null): the registry entry with the lowest `cost_per_1k_tokens` whose `capabilities` include `chat` (or empty capabilities = assume chat), excluding the agent's own primary. No qualifying entry → Smart is inert for that agent (candidates unchanged) — fail-expensive, never fail-broken.
- Per-agent override: `AgentProfile.routing` (existing `Option<RoutingConfig>`) — `RoutingConfig` gains the same optional `smart` sub-block; per-agent → global precedence as everywhere else.
- Hub `model_switch_get/set` (Phase 2) carries the new field automatically (full-object set); ref validation extends to `smart.cheap`.

### 3. Smart candidate assembly (`FallbackLlmClient::candidates_for`)

```
if req.intent is Background(_) and smart_effective(profile, cfg).enabled:
    cheap = smart.cheap or auto_pick(registry, primary)
    if cheap exists:
        candidates = dedup([cheap] + phase1_candidates)   // phase1 = [primary, …chain]
        → cheap first; the agent's primary is the LAST resort (cascade target)
else:
    candidates = phase1_candidates                         // byte-for-byte Phase 1/2 behavior
```

The Phase-1 loop (per-candidate retry with backoff, cooldown, advance-on-retryable) is reused as-is; cascade is simply "cheap sits at the head, better models behind it".

### 4. Cascade semantics (Background+Smart only)

- **Escalation triggers** (advance to next candidate): the existing retryables (429/5xx/timeout/402) **plus, only under Background+Smart, `InvalidResponse`** (malformed/empty output — a structural quality failure of the cheap model).
- **Interactive turns are untouched**: `InvalidResponse` remains Fatal there (Phase-1 security boundary — a malformed response on the user's chosen model must surface, not silently switch models).
- Implementation: `classify()` keeps its Phase-1 signature/behavior; the generate loop consults `(classify(e), req.intent, smart_active)` — a second, additive match, not a change to `classify`.
- **Escalation cap**: at most `max_escalations` structural escalations per task (counted per `task_id` in the client's in-memory state, same lifetime as the cooldown map). Beyond the cap, remaining candidates are only tried for Phase-1 retryable errors.
- 401/400 stay Fatal everywhere. Auth failures never trigger switching — unchanged from Phase 1.

### 5. Outcome recording (Phase B — `mur.routing` events)

`FallbackLlmClient` gains an optional `telemetry: Option<mpsc::Sender<Event>>` (injected in `build_provider_runner` from the runtime's existing `TelemetryWriter`). After every `generate()` completes (success or final error), emit one JSONL event:

```json
{
  "kind": "mur.routing",
  "ts": "2026-07-13T…Z",
  "agent": "coach",
  "task_id": "task-…",
  "intent": "background/scheduled | background/companion | background/maintenance | interactive",
  "model_ref": "deepseek_v4_flash",
  "reason": "smart-background | explicit | fallback-advance | escalated",
  "outcome": "ok | structural_fail | escalated | error | user_corrected",
  "attempts": 1,
  "escalations": 0,
  "input_tokens": 1234,
  "output_tokens": 567,
  "task_summary": "<first 200 chars of the task's user text>"
}
```

- Written to the agent's existing daily telemetry JSONL (`~/.mur/agents/<name>/telemetry/<date>.jsonl`) — same privacy class as ambient session capture (local, never leaves the machine).
- `task_summary` is raw text (truncated at a documented const, 200 chars); embeddings are computed later by Phase C at index time, not now.
- Recording is **on for all intents** (interactive included) — that is the point of Phase B: interactive data is what earns Phase-C automation.
- Failure to send telemetry never fails the request (best-effort, `try_send`/ignore).
- `user_corrected` events are emitted by the re-run path (below), referencing the original `task_id`.

### 6. Transparency + one-click correction (Hub chat)

- The runtime's per-turn usage payload (`Task.usage` → Hub) gains `model_ref: String` and `route_reason: String`.
- Hub chat renders a small caption under each agent reply: `⚡ deepseek_v4_flash · Smart (background)`; interactive replies show just the model name. Hover shows the reason.
- Replies produced by a Smart downgrade also render **"↑ re-run on smart model"**. Mechanism: the Hub re-issues the turn via the normal A2A `message/send` with a `pin_model_ref` entry in the message metadata; the runtime threads it into the request as `LlmRequest.pin_model_ref: Option<String>` (defaulted `None`, sibling of `intent`), and `candidates_for` returns exactly `[pinned]` when set (validated against the registry; invalid → ignored, normal candidates used). The reply replaces/appends in the chat, and the runtime records `outcome:"user_corrected"` referencing the original `task_id` — the gold training signal for Phase C.
- murmur TUI: no new UI in this phase; the routing events are queryable from telemetry.

### 7. Settings UI (Hub, extends Phase 2's MODEL SWITCHING section)

- **Smart mode** toggle (default on) + plain-language copy: "Background tasks run on a cost-saving model and escalate automatically if they fail."
- **Cheap model** `ModelRefSelect` (empty = "auto (cheapest chat model)").
- Existing fail-closed validation extends to `smart.cheap`.

## Error handling

- Auto-pick finds no candidate → Smart inert (log once at info); behavior = Phase 1.
- `smart.cheap` ref deleted from registry after being set → `candidates_for` lookup fails for that ref → factory error path already records `last` and advances (Phase-1 behavior) → effectively skips to primary. Hub validation prevents most of this at write time.
- Telemetry channel full/closed → drop event silently (never block or fail a turn).
- Escalation-cap state is in-memory per runtime process (restart resets — acceptable, mirrors cooldown map).

## Testing

- `RequestIntent` default = `Interactive`; untagged call sites behave byte-for-byte as Phase 1/2 (regression).
- Background+Smart candidate order `[cheap, …chain, primary]`, deduped; Smart disabled/inert → Phase-1 candidates.
- Cascade: `InvalidResponse` advances under Background+Smart; stays Fatal under Interactive; `max_escalations` cap enforced; 401/400 Fatal everywhere.
- Auto-pick: lowest-cost chat-capable entry, excludes primary, none-found → inert.
- Recording: one `mur.routing` event per generate with correct intent/outcome/tokens; telemetry failure doesn't fail the turn; `user_corrected` emitted on re-run.
- Config: legacy `models:` without `smart:` → enabled=true/auto-pick; per-agent smart override wins over global.
- Hub UI: helper unit tests (vitest) + `npm run build`; visual QA via Hub rebuild (Phase-2 pattern).

## Rollout & phasing

- **Smart is ON by default** for background turns — this is the product stance (automation is MUR's default), made safe by: small blast radius (background only), cascade to the agent's own model, mandatory transparency, and one-click correction. Global toggle + per-agent override for opt-out.
- Recording ships with no user-facing toggle (same posture as ambient capture; local-only).
- Implementation order: runtime core (intent/candidates/cascade/recording) → usage payload fields → Hub UI (settings + chat caption + re-run). Each lands independently testable.
- **Phase C (later, separate spec)**: kNN memory router over recorded outcomes (LanceDB embeddings of `task_summary`), earning interactive-turn automation from real data.

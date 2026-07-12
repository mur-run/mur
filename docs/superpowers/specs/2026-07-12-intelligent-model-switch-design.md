# Intelligent Model Switching — Design Spec

> **Date**: 2026-07-12
> **Status**: Ready for review
> **Scope**: A minimal, config-layered model-selection + failure-fallback layer for MUR agents (global + per-agent settings, priority per-agent → global), managed from both the CLI and the MUR Hub GUI. Opt-in difficulty routing. Does NOT implement the learned/RouteLLM router — that stays cost-router Phase 3.

## Overview

MUR agents already bind a model via `AgentProfile.model_ref` (looked up in `~/.mur/models.yaml`) with a runtime `--model` override. This spec adds two capabilities on top, both driven by config:

1. **Config-layered model resolution** — a global default model, overridable per-agent, so an agent without an explicit `model_ref` falls back to a workspace default instead of the hard-coded inline `model:` block.
2. **Failure fallback chains** — an ordered list of models tried in sequence when a call fails with a *retryable* error (rate-limit / 5xx / timeout / insufficient-credit), with per-model retry (exponential backoff + jitter) and an in-memory cooldown (circuit-breaker) so a failing model is skipped while it recovers.

An **opt-in difficulty heuristic** (off by default) selects between a "cheap" and a "frontier" model by estimated input-token count before resolution.

This is the Phase-1 MVP identified by the 2026 deep-research pass (fleet `deep-research`, 2026-07-12): *"start with per-agent defaults + per-task overrides"* and *"Phase 1 = heuristic router + fallback chain"*. Learned routing (RouteLLM-style classifier) is explicitly deferred — it requires production sample collection and is out of scope.

### Relationship to the Cost-Router spec

`docs/superpowers/specs/2026-06-01-cost-router-orchestrator-design.md` Phase 1 ("Router on the model registry") describes a fuller hybrid auto+override router. This spec is a **lightweight precursor**, not that Router: it shares the `models.yaml` registry but ships only config-layered resolution + fallback + a single opt-in heuristic. The learned Router remains cost-router's later phase. Keeping them separate avoids scope creep.

### Non-goals

- Learned / classifier-based routing (RouteLLM, NotDiamond). Deferred to cost-router Phase 3.
- Persistent cooldown state across process restarts (in-memory only).
- Mid-session quality-based re-routing (observing output quality and switching). Out of scope.
- Per-request model override beyond the existing runtime `--model` flag.

## Architecture

Two units, both feeding the single existing resolution point `resolve_model_entry` in `mur-agent-runtime/src/supervisor.rs` (~line 1148):

```
                    ┌──────────────────────────┐
 prompt/message ──▶ │ difficulty heuristic      │ (opt-in, off by default)
                    │ (token count → cheap/     │
                    │  frontier model_ref)      │
                    └───────────┬──────────────┘
                                │ chosen primary ref (or profile.model_ref)
                                ▼
                    ┌──────────────────────────┐
                    │ ModelResolver             │  priority: per-agent → global
                    │ → ordered Vec<candidate>  │  [primary, ...fallback_chain]
                    └───────────┬──────────────┘
                                ▼
                    ┌──────────────────────────┐
   LLM call ──────▶ │ FallbackExecutor          │  per-candidate retry (backoff+jitter),
                    │  loop over candidates,     │  classify error, cooldown map,
                    │  skip cooled-down models   │  advance on retryable failure
                    └──────────────────────────┘
```

- **`ModelResolver`** — pure function: `(profile, global_config) → Vec<ModelCandidate>`. Testable in isolation.
- **`FallbackExecutor`** — a `FallbackLlmClient` that itself implements the runtime's `LlmClient` trait (`#[async_trait]`, used as `Arc<dyn LlmClient>`, so object-safe). It holds the ordered candidates + a client factory + the in-memory cooldown map, and its `generate` loops over candidates. Because it *is* an `LlmClient`, it drops into the existing `Arc<dyn LlmClient>` slot with no agent-loop changes.
- **Difficulty heuristic** — pure function `(estimated_input_tokens, routing_config) → model_ref`.

## Data Model

### Global config (`~/.mur/config.yaml`) — new `models:` block

New `Config.models: ModelSwitchConfig` (separate from the existing `llm:` block, per review decision):

```yaml
models:
  default: claude_sonnet          # global default when an agent has no model_ref
  fallback_chain:                 # tried in order on retryable failure
    - claude_sonnet
    - deepseek_v4_pro
  retry:
    max_retries: 1                # per-candidate retries before advancing the chain
    backoff_base_ms: 500          # exponential base; delay = base * 2^attempt + jitter
    cooldown_secs: 60             # circuit-breaker: skip a failed model this long
  routing:                        # opt-in difficulty routing
    enabled: false
    cheap: deepseek_v4_flash
    frontier: claude_opus
    threshold_input_tokens: 2000
```

```rust
// mur-common/src/config.rs
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelSwitchConfig {
    pub default: Option<String>,          // global default model_ref
    #[serde(default)]
    pub fallback_chain: Vec<String>,      // global fallback chain
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_retries: u32,        // default DEFAULT_MAX_RETRIES
    pub backoff_base_ms: u64,    // default DEFAULT_BACKOFF_BASE_MS
    pub cooldown_secs: u64,      // default DEFAULT_COOLDOWN_SECS
}
// impl Default reads the module consts below.

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoutingConfig {
    #[serde(default)]
    pub enabled: bool,
    pub cheap: Option<String>,
    pub frontier: Option<String>,
    #[serde(default)]
    pub threshold_input_tokens: Option<u32>,   // default DEFAULT_ROUTING_THRESHOLD when routing enabled
}
```

Documented default constants (no hard-coded literals at call sites — Mandatory Rule 1):

```rust
pub const DEFAULT_MAX_RETRIES: u32 = 1;
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 500;
pub const DEFAULT_COOLDOWN_SECS: u64 = 60;
pub const DEFAULT_ROUTING_THRESHOLD: u32 = 2000;
```

### Per-agent (`AgentProfile`) — one new optional field

`model_ref: Option<String>` already exists. Add a per-agent fallback chain:

```rust
// mur-common/src/agent.rs (AgentProfile)
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub fallback_chain: Vec<String>,   // per-agent override of the global chain
```

Optional per-agent routing override reuses `RoutingConfig`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub routing: Option<RoutingConfig>,
```

Legacy profiles without these fields must still deserialize (both are `#[serde(default)]`) — a regression test guards this, mirroring the existing `legacy_profile_without_model_ref_still_parses` test.

## Resolution Priority (per-agent → global)

`ModelResolver::resolve(profile, cfg) -> Vec<ModelCandidate>` where `ModelCandidate` carries a `model_ref: String` and its resolved `ModelEntry`.

| Decision | Source, highest priority first |
|---|---|
| Primary model | (routing heuristic result, if routing enabled) → per-agent `model_ref` → global `models.default` → existing inline `model:` block |
| Fallback chain | per-agent `fallback_chain` (if non-empty) → global `models.fallback_chain` → empty |
| Routing config | per-agent `routing` (if `Some`) → global `models.routing` → disabled |

Rules:
- The primary is always the head of the returned vector.
- The fallback chain is appended after the primary; the primary is de-duplicated out of the chain if it also appears there (no point retrying the same ref back-to-back).
- If neither per-agent nor global supplies anything, the vector is a single candidate = today's behavior (inline model). This preserves existing agents byte-for-byte.

## Fallback Execution

`FallbackExecutor` wraps the runtime's LLM invocation:

```
for candidate in resolver.resolve(profile, cfg):
    if cooldown.is_cooling(candidate.model_ref): continue
    for attempt in 0..=cfg.retry.max_retries:
        result = call_model(candidate)               # builds client per candidate
        match classify(result):
            Ok(resp)            => return Ok(resp)
            Err(Fatal(e))       => return Err(e)      # auth/malformed: do NOT fallback
            Err(Retryable(_)):
                if attempt < max_retries:
                    sleep(backoff(attempt))           # base * 2^attempt + jitter
                else:
                    cooldown.mark(candidate.model_ref, now + cooldown_secs)
                    break                              # advance to next candidate
return Err(last_error)                                # chain exhausted; surface it
```

### Error classification

```rust
pub enum CallOutcome { Ok(Response), Retryable(RetryKind), Fatal(anyhow::Error) }
pub enum RetryKind { RateLimit, ServerError, Timeout, InsufficientCredit }
```

- **Retryable** → advance/retry: HTTP `429` (RateLimit), `5xx` (ServerError), connect/read timeout (Timeout), `402` / provider "insufficient credit/quota" (InsufficientCredit).
- **Fatal** → do NOT fallback: `400` (bad request), `401`/`403` (auth), malformed response, schema violation. Falling back on these hides a real bug or misconfiguration.

### Cooldown map (circuit-breaker)

In-memory `HashMap<String /*model_ref*/, Instant /*cooling_until*/>` owned by the executor (per runtime process). `is_cooling` returns true while `now < cooling_until`. No persistence — a restart clears it (acceptable; cooldowns are seconds-scale).

## Difficulty Heuristic (opt-in)

When `routing.enabled`:

```rust
fn choose_by_difficulty(est_input_tokens: u32, r: &RoutingConfig) -> Option<String> {
    let threshold = r.threshold_input_tokens.unwrap_or(DEFAULT_ROUTING_THRESHOLD);
    match (r.cheap.as_ref(), r.frontier.as_ref()) {
        (Some(cheap), Some(frontier)) =>
            Some(if est_input_tokens > threshold { frontier.clone() } else { cheap.clone() }),
        _ => None,   // misconfigured → fall through to model_ref/global default
    }
}
```

`est_input_tokens` is estimated from the message text (a coarse chars/4 approximation is sufficient — this is a routing heuristic, not billing). If routing is disabled or misconfigured, resolution proceeds with `model_ref`/global default unchanged.

## CLI Surface

- `mur model default <ref>` — set/clear global `models.default` (writes `config.yaml`).
- `mur model fallback <ref>...` — set the global `models.fallback_chain` (ordered; empty clears).
- Per-agent `fallback_chain` is written by extending the existing per-agent model-binding path (`mur-core/src/cmd/agent/model_resolve.rs`), e.g. a `--fallback <ref>...` companion to the existing model_ref setter. Difficulty routing is config-only in v1 (no dedicated CLI); users edit `config.yaml`.

All CLI writes validate that each `<ref>` exists in `models.yaml` and error otherwise (fail-closed).

## Hub GUI Management (Phase 2)

The feature must be manageable from the MUR Hub, not only the CLI. Both surfaces already exist in `mur-hub-gui`, so this is thin wiring over the same core logic — **not** a new subsystem. It depends on Phase 1 (core + CLI) landing first; the Hub commands are wrappers over the same `store::config` read/write and resolver used by the CLI.

**Global scope — Settings → Models tab** (`ui/src/components/settings/ModelsSettings.tsx`; the Hub already reads/writes `config.yaml` via `mur_core::store::config::{load_config,save_config}`, as the fleet-settings commands do):
- A **Default model** combobox (reuses the existing `ModelCombobox`, populated from `list_models`).
- A **Fallback chain** ordered list editor (add/remove/reorder model refs from the registry).
- A **Difficulty routing** section: an enable toggle + cheap/frontier comboboxes + threshold input, defaulting to disabled.
- New Tauri commands `model_switch_get() -> ModelSwitchView` and `model_switch_set(patch)` wrapping `load_config`/`save_config`, mirroring the existing `agent_get_notif_config`/`agent_set_notif_config` pattern in `notif.rs`. Each ref is validated against `models.yaml` (fail-closed) before save.

**Per-agent scope — agent detail model section** (the existing per-agent picker: `ModelPickerModal.tsx` / `ModelCombobox.tsx`, backed by `apply_agent_model` / `set_concierge_model_ref`):
- Below the existing primary-model picker, add a **per-agent fallback chain** editor and an optional **routing override** (inherits global when unset).
- Extend the existing per-agent model command (or add `agent_set_fallback_chain(name, refs)`) to write the profile's `fallback_chain` / `routing` fields.
- Empty per-agent settings inherit the global defaults — the UI shows the effective (inherited) value greyed until overridden, matching the resolution priority (per-agent → global).

**Phasing:** Phase 1 ships core + CLI (testable headless). Phase 2 adds the Hub commands + UI. Because mur-hub-gui is workspace-excluded (Tauri) and built separately, splitting it keeps Phase 1 shippable and CI-verifiable without the GUI toolchain. Phase 2 is a separate implementation plan.

## Error Handling & Observability

- Every fallback advance and cooldown mark emits a `tracing` event at `info` (model_ref, RetryKind, attempt) so operators can see switching in logs — mirrors the existing `egress proxy CONNECT` audit style.
- Chain exhaustion returns the last candidate's error with context listing the models tried.
- A fatal error short-circuits with the original error (no chain noise).

## Testing

- **Resolver priority**: per-agent `model_ref` overrides global `default`; per-agent `fallback_chain` overrides global; empty both → single inline candidate (existing behavior). Primary de-dup out of chain.
- **Legacy profile**: a profile YAML without `fallback_chain`/`routing` still deserializes.
- **Error classifier**: 429/5xx/timeout/402 → Retryable(kind); 400/401/malformed → Fatal.
- **FallbackExecutor** (mock client): fails N times then succeeds → advances the chain the right number of times; a Fatal error returns immediately without advancing; a model in cooldown is skipped; chain exhaustion returns the last error.
- **Cooldown**: `is_cooling` true within window, false after; `mark` sets the window from `cooldown_secs`.
- **Difficulty heuristic**: `est > threshold → frontier`, `est <= threshold → cheap`, misconfigured (missing cheap/frontier) → `None` (fall-through).
- **Config defaults**: omitted `retry`/`routing` fields deserialize to the documented default constants.

## Rollout

Additive and off-by-default: with an empty `models:` block and no per-agent `fallback_chain`, `resolve` returns a single candidate and `FallbackExecutor` degenerates to today's single call — existing agents are unaffected. Difficulty routing ships `enabled: false`.

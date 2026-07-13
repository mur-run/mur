# Intelligent Switching — Phase A+B Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Auto-route background agent turns to a cheap model (cascade-escalating on structural failure, fully transparent, one-click correctable), and record every routed turn's outcome locally as training data for the future memory router.

**Architecture:** An additive `RequestIntent` tag on `LlmRequest` (defaulted `Interactive`) lets the four generate call sites declare context in one line; the decision, cascade, and recording all live in the existing Phase-1 `FallbackLlmClient` choke point. Interactive turns keep Phase-1/2 behavior byte-for-byte. A new `mur.routing` telemetry event records outcomes for all intents. The Hub surfaces the decision + a re-run control.

**Tech Stack:** Rust (edition 2024, `#[async_trait]`, serde, tokio mpsc), React/TypeScript (Vite, vitest), Hub Tauri.

## Global Constraints

- **Additive & Phase-1-safe.** New `LlmRequest` fields default so old code compiles/behaves identically. The retryable/fatal boundary for **Interactive** turns is UNCHANGED (`InvalidResponse` stays Fatal there — Phase-1 security boundary). Only **Background+Smart** treats `InvalidResponse` as escalatable.
- **Fail-expensive.** When Smart can't pick a cheap model (no qualifying registry entry), Smart is inert → Phase-1 candidates. Never fail-broken.
- **Explicit beats smart.** `pin_model_ref` (re-run) and per-agent `model_ref` / fleet delegate are never overridden by Smart.
- **Smart default = ON** for background turns (`SmartConfig.enabled` serde-default `true`). Global toggle + per-agent override.
- **Transparency mandatory.** Every routed turn emits a `mur.routing` event; the Hub shows model + reason. Recording is best-effort (never fails a turn) and local-only (agent telemetry JSONL, same privacy class as ambient capture).
- **No hardcoded values** — `DEFAULT_SMART_MAX_ESCALATIONS`, truncation length are documented consts.
- Rust edition 2024; comments/strings English (Hub copy via `useT()`); files ≤ 800 lines.
- **Build/test env:** `mur-common`/`mur-agent-runtime` need no special env; `mur-core` needs `ORT_STRATEGY=download` + `MUR_WEB_DIST=$HOME/Projects/mur-web/dist`. Hub Rust: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml`. Hub UI: `cd mur-hub-gui/ui && npm test` / `npm run build`. Add `~/.rustup/toolchains/stable-*/bin` to PATH if `cargo` missing. Run `cargo fmt` before every commit.

## File Structure

- `mur-agent-runtime/src/llm/mod.rs` — `RequestIntent`/`BackgroundKind`; `LlmRequest` gains `intent`, `pin_model_ref`, `task_id`. (T1)
- `mur-common/src/config.rs` — `SmartConfig` + `ModelSwitchConfig.smart` + `RoutingConfig.smart`. (T2)
- `mur-common/src/model.rs` — `pick_cheap_model` auto-pick helper. (T2)
- `mur-agent-runtime/src/llm/fallback.rs` — `candidates_for` Smart/pin logic (T3); cascade classification (T4); telemetry emit (T5).
- `mur-agent-runtime/src/telemetry_writer.rs` — `Event::Routing` + writer handler. (T5)
- `mur-agent-runtime/src/supervisor_runner.rs` + call sites — telemetry injection + intent tagging + usage fields. (T5, T6)
- `mur-hub-gui/src-tauri/src/model_switch.rs` — smart-field validation passes through (T7).
- `mur-hub-gui/ui/src/components/settings/ModelsSettings.tsx` + `modelSwitch.ts` — Smart toggle. (T7)
- `mur-hub-gui/ui/...chat...` — caption + re-run. (T8)

---

## Phase A+B runtime core (headless-testable)

### Task 1: `RequestIntent` + `LlmRequest` fields

**Files:** Modify `mur-agent-runtime/src/llm/mod.rs`

**Interfaces:**
- Produces: `RequestIntent { Interactive (default), Background(BackgroundKind) }`, `BackgroundKind { Scheduled, Companion, Maintenance }`; `LlmRequest.intent: RequestIntent`, `LlmRequest.pin_model_ref: Option<String>`, `LlmRequest.task_id: Option<String>`.

- [ ] **Step 1: Write the failing test** (in `llm/mod.rs` tests)

```rust
#[test]
fn llm_request_intent_defaults_interactive() {
    let r = LlmRequest::default();
    assert_eq!(r.intent, RequestIntent::Interactive);
    assert!(r.pin_model_ref.is_none());
    assert!(r.task_id.is_none());
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p mur-agent-runtime llm_request_intent_defaults` → FAIL (types missing).

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    Scheduled,
    Companion,
    Maintenance,
}

/// Why this LLM call is being made. Interactive = user-facing (chat, A2A send,
/// fleet delegate); Background = runtime-initiated, nobody watching live —
/// eligible for Smart cheap-model routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestIntent {
    #[default]
    Interactive,
    Background(BackgroundKind),
}
```

Add to `LlmRequest` (keep the existing `#[derive(Debug, Clone, Default)]`):

```rust
    /// Routing context; defaults to Interactive (see RequestIntent).
    pub intent: RequestIntent,
    /// Force exactly this model_ref (user "re-run on smart model"); bypasses
    /// Smart/fallback candidate assembly. None = normal resolution.
    pub pin_model_ref: Option<String>,
    /// Owning task id, threaded for telemetry correlation. None outside tasks.
    pub task_id: Option<String>,
```

- [ ] **Step 4: Run to verify it passes** — `cargo test -p mur-agent-runtime llm_request_intent_defaults` then `cargo build -p mur-agent-runtime` (all existing `LlmRequest { … }` literals still compile because the new fields have defaults ONLY via `..Default::default()`; if any literal doesn't use spread, it will fail here — fix by adding the three fields or `..Default::default()`). Expected: PASS + builds.

- [ ] **Step 5: Commit** — `git commit -m "feat(llm): RequestIntent + pin_model_ref/task_id on LlmRequest (additive, defaults)"`

---

### Task 2: `SmartConfig` + auto-pick cheap model

**Files:** Modify `mur-common/src/config.rs`, `mur-common/src/model.rs`

**Interfaces:**
- Produces: `SmartConfig { enabled: bool, cheap: Option<String>, max_escalations: u32 }`; `ModelSwitchConfig.smart: SmartConfig`; `RoutingConfig.smart: Option<SmartConfig>`; const `DEFAULT_SMART_MAX_ESCALATIONS = 1`; `pub fn pick_cheap_model(reg: &ModelRegistry, exclude: Option<&str>) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

`config.rs`:
```rust
#[test]
fn smart_config_defaults_on_with_autopick() {
    let cfg: Config = serde_yaml::from_str("{}").unwrap();
    assert!(cfg.models.smart.enabled);              // default ON
    assert_eq!(cfg.models.smart.cheap, None);        // auto-pick
    assert_eq!(cfg.models.smart.max_escalations, DEFAULT_SMART_MAX_ESCALATIONS);
}
```

`model.rs`:
```rust
#[test]
fn pick_cheap_model_lowest_cost_chat_excluding_primary() {
    let mut reg = ModelRegistry::default();
    let mk = |cost: f64, caps: &[&str]| ModelEntry {
        provider: "x".into(), model: "m".into(),
        capabilities: caps.iter().map(|s| s.to_string()).collect(),
        cost_per_1k_tokens: Some(cost), ..Default::default()
    };
    reg.models.insert("frontier".into(), mk(0.01, &["chat"]));
    reg.models.insert("cheap".into(),    mk(0.0001, &["chat"]));
    reg.models.insert("embed".into(),    mk(0.00001, &["embedding"])); // not chat → skip
    // cheapest chat-capable, excluding the agent's own primary:
    assert_eq!(pick_cheap_model(&reg, Some("cheap")), Some("frontier".into())); // cheap excluded
    assert_eq!(pick_cheap_model(&reg, None), Some("cheap".into()));
    // no chat entries → None (Smart inert)
    let mut empty = ModelRegistry::default();
    empty.models.insert("e".into(), mk(0.0, &["embedding"]));
    assert_eq!(pick_cheap_model(&empty, None), None);
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p mur-common smart_config_defaults pick_cheap_model` → FAIL.

- [ ] **Step 3: Implement**

`config.rs` (near the Phase-1 model-switch structs):
```rust
pub const DEFAULT_SMART_MAX_ESCALATIONS: u32 = 1;
fn default_smart_max_escalations() -> u32 { DEFAULT_SMART_MAX_ESCALATIONS }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmartConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,
    #[serde(default = "default_smart_max_escalations")]
    pub max_escalations: u32,
}
impl Default for SmartConfig {
    fn default() -> Self {
        Self { enabled: true, cheap: None, max_escalations: DEFAULT_SMART_MAX_ESCALATIONS }
    }
}
```

(Confirm a `default_true` fn exists in `config.rs` — Phase 1/other configs use it; if not, add `fn default_true() -> bool { true }`.)

Add `#[serde(default)] pub smart: SmartConfig` to `ModelSwitchConfig`, and `#[serde(default, skip_serializing_if = "Option::is_none")] pub smart: Option<SmartConfig>` to `RoutingConfig`. (`RoutingConfig` already derives `PartialEq` — Phase 2; `SmartConfig` derives it too so the nested field is fine.)

`model.rs`:
```rust
/// Pick the cheapest chat-capable model_ref for Smart background routing,
/// excluding `exclude` (the agent's own primary). Chat-capable = capabilities
/// contains "chat" OR is empty (legacy entries assumed chat). None when no
/// qualifying entry exists → caller keeps normal candidates (fail-expensive).
pub fn pick_cheap_model(reg: &ModelRegistry, exclude: Option<&str>) -> Option<String> {
    reg.models
        .iter()
        .filter(|(k, _)| exclude != Some(k.as_str()))
        .filter(|(_, e)| e.capabilities.is_empty() || e.capabilities.iter().any(|c| c == "chat"))
        .filter_map(|(k, e)| e.cost_per_1k_tokens.map(|c| (c, k.clone())))
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, k)| k)
}
```

- [ ] **Step 4: Run to verify they pass** — `cargo test -p mur-common smart_config_defaults pick_cheap_model` then `cargo test -p mur-common config:: model::`. Expected: PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(config): SmartConfig (default-on) + pick_cheap_model auto-pick"`

---

### Task 3: Smart candidate assembly in `FallbackLlmClient`

**Files:** Modify `mur-agent-runtime/src/llm/fallback.rs`

**Interfaces:**
- Consumes: `RequestIntent` (T1), `SmartConfig`/`pick_cheap_model` (T2), Phase-1 `candidates_for`/`resolve_model_refs`.
- Produces: `candidates_for(req)` now honors `pin_model_ref`, and under `Background + smart.enabled` prepends the cheap model.

**Context:** Phase-2 `candidates_for(&self, req: &LlmRequest) -> Vec<String>` matches `CandidateSource::{Static, PerRequest{profile, cfg}}`. This task adds two branches BEFORE the existing logic.

- [ ] **Step 1: Write the failing test** (in `fallback.rs` tests, alongside Phase-2's)

```rust
#[test]
fn candidates_pin_overrides_everything() {
    // A pinned ref returns exactly [pinned] regardless of source.
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(Default::default()), retry0());
    let mut req = LlmRequest::default();
    req.pin_model_ref = Some("frontier".into());
    assert_eq!(fb.candidates_for(&req), vec!["frontier".to_string()]);
}

#[test]
fn candidates_smart_background_prepends_cheap() {
    use mur_common::agent::AgentProfile;
    use mur_common::config::{ModelSwitchConfig, SmartConfig};
    let mut cfg = ModelSwitchConfig::default();
    cfg.default = Some("primary".into());
    cfg.fallback_chain = vec!["primary".into(), "mid".into()];
    cfg.smart = SmartConfig { enabled: true, cheap: Some("cheap".into()), max_escalations: 1 };
    let fb = FallbackLlmClient::new_routed(AgentProfile::default_for_tests(), cfg, factory_for(Default::default()), retry0());
    // Background + smart → cheap first, then phase-1 candidates, deduped.
    let mut bg = LlmRequest::default();
    bg.intent = RequestIntent::Background(BackgroundKind::Scheduled);
    assert_eq!(fb.candidates_for(&bg), vec!["cheap".to_string(), "primary".into(), "mid".into()]);
    // Interactive → unchanged (no cheap prepend).
    let inter = LlmRequest::default();
    assert_eq!(fb.candidates_for(&inter), vec!["primary".to_string(), "mid".into()]);
}
```

(`candidates_for` may need to be `pub(crate)` for the test — it likely already is or is testable in-module; keep it in-module.)

- [ ] **Step 2: Run to verify it fails** — `cargo test -p mur-agent-runtime candidates_pin candidates_smart` → FAIL.

- [ ] **Step 3: Implement** — at the top of `candidates_for`, before the `match &self.source`:

```rust
        // 1. Explicit pin (user re-run) wins over everything.
        if let Some(p) = &req.pin_model_ref {
            return vec![p.clone()];
        }
```

Then in the `CandidateSource::PerRequest { profile, cfg }` arm, after computing the Phase-1 `base` candidates (the existing `resolve_model_refs(...)` result), add the Smart prepend:

```rust
            CandidateSource::PerRequest { profile, cfg } => {
                // (existing Phase-1/2 routing → `base` : Vec<String>)
                let base = /* existing resolve_model_refs(...) result */;
                // Smart background: cheap model first, base behind it (cascade).
                let smart = profile.routing.as_ref().and_then(|r| r.smart.clone())
                    .unwrap_or_else(|| cfg.smart.clone());
                if matches!(req.intent, RequestIntent::Background(_)) && smart.enabled {
                    let primary = base.first().cloned();
                    let cheap = smart.cheap.clone().or_else(|| {
                        // auto-pick from the registry, excluding the primary
                        mur_common::model::ModelRegistry::load_from(
                            &mur_common::model::ModelRegistry::default_path().ok()?,
                        ).ok().and_then(|reg| mur_common::model::pick_cheap_model(&reg, primary.as_deref()))
                    });
                    if let Some(c) = cheap {
                        let mut out = vec![c];
                        for r in base { if !out.contains(&r) { out.push(r); } }
                        return out;
                    }
                }
                base
            }
```

(Adapt to the exact existing `PerRequest` body — read it first; the only additions are the `smart`/`cheap`/prepend block. The `.ok()?` in a non-Option closure won't compile — use a helper `fn autopick(primary) -> Option<String>` or inline with explicit matches. Write it so it compiles; the intent is: registry load failure or no cheap → fall through to `base`.)

- [ ] **Step 4: Run to verify it passes** — `cargo test -p mur-agent-runtime candidates_pin candidates_smart fallback::` (Phase-1/2 candidate tests still pass). Expected: PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(llm): Smart background candidate assembly + pin override in FallbackLlmClient"`

---

### Task 4: Cascade — escalate on structural failure (Background+Smart only)

**Files:** Modify `mur-agent-runtime/src/llm/fallback.rs`

**Interfaces:**
- Consumes: Phase-1 `classify`/`Retryability`, `RequestIntent`, `SmartConfig.max_escalations`.
- Produces: the `generate` loop advances past a candidate on `InvalidResponse` ONLY under Background+Smart, capped at `max_escalations` per task; Interactive `InvalidResponse` stays Fatal.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn cascade_escalates_structural_fail_under_background_smart() {
    // cheap returns InvalidResponse (structural), then primary succeeds.
    let mut scripts = std::collections::HashMap::new();
    scripts.insert("cheap".into(), vec![Err(LlmError::InvalidResponse("empty".into()))]);
    scripts.insert("primary".into(), vec![Ok(())]);
    // Build a routed client whose candidates_for yields [cheap, primary] for background.
    let fb = /* new_routed with smart cheap=cheap, default=primary, max_escalations=1 */;
    let mut bg = LlmRequest::default();
    bg.intent = RequestIntent::Background(BackgroundKind::Scheduled);
    assert_eq!(fb.generate(bg).await.unwrap().text, "primary"); // escalated to primary
}

#[tokio::test]
async fn interactive_invalid_response_stays_fatal() {
    let mut scripts = std::collections::HashMap::new();
    scripts.insert("a".into(), vec![Err(LlmError::InvalidResponse("x".into()))]);
    scripts.insert("b".into(), vec![Ok(())]);
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(scripts), retry0());
    // Interactive: InvalidResponse is Fatal → returns the error, never tries b.
    let err = fb.generate(LlmRequest::default()).await.unwrap_err();
    assert!(matches!(err, LlmError::InvalidResponse(_)));
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p mur-agent-runtime cascade_escalates interactive_invalid_response` → the cascade test fails (InvalidResponse currently Fatal always).

- [ ] **Step 3: Implement** — in the `generate` loop, where the error is classified, add the Background+Smart structural-escalation branch. Do NOT change `classify`; add a local decision:

```rust
    // Track structural escalations for this call (cap per task).
    let mut escalations = 0u32;
    let max_esc = /* smart max_escalations for this req; 0 when not smart/interactive */;
    // ... inside the per-candidate error handling:
    match classify(&e) {
        Retryability::Fatal => {
            // Structural failure is escalatable ONLY under Background+Smart, within cap.
            let structural = matches!(e, LlmError::InvalidResponse(_));
            let smart_bg = matches!(req.intent, RequestIntent::Background(_)) && max_esc > 0;
            if structural && smart_bg && escalations < max_esc {
                escalations += 1;
                tracing::info!(model_ref, "smart cascade: structural fail, escalating");
                last = Some(e);
                break; // advance to next candidate (the better model)
            }
            return Err(e); // Interactive / over-cap / non-structural Fatal → surface
        }
        Retryability::Retryable => { /* unchanged Phase-1 backoff/cooldown/advance */ }
    }
```

Compute `max_esc` from the effective SmartConfig for this request (per-agent → global), 0 when the client isn't routed or Smart disabled or intent Interactive. Record `escalations` for the telemetry event (T5).

- [ ] **Step 4: Run to verify it passes** — `cargo test -p mur-agent-runtime cascade interactive_invalid fallback::` (Phase-1 fatal-boundary test `fatal_error_does_not_advance` still passes — it uses `Http("401")` which is non-structural Fatal, unaffected). Expected: PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(llm): cascade escalate-on-structural-failure under Background+Smart (Interactive unchanged)"`

---

### Task 5: `mur.routing` recording + telemetry injection

**Files:** Modify `mur-agent-runtime/src/telemetry_writer.rs`, `mur-agent-runtime/src/llm/fallback.rs`, `mur-agent-runtime/src/llm/client_builder.rs`/`supervisor_runner.rs`

**Interfaces:**
- Produces: `Event::Routing { … }` variant + writer handler emitting `mur.routing` JSONL; `FallbackLlmClient` holds `Option<mpsc::Sender<Event>>` and emits one event per `generate`.
- Consumes: `TelemetryWriter::sender()` (existing), `RequestIntent`, outcome from the generate loop.

- [ ] **Step 1: Write the failing test** (telemetry emit is easiest to unit-test at the event-serialization layer)

```rust
// in telemetry_writer.rs tests: a Routing event serializes with the expected fields.
#[test]
fn routing_event_serializes_fields() {
    let ev = Event::Routing {
        agent: "coach".into(), task_id: Some("t1".into()),
        intent: "background/scheduled".into(), model_ref: "cheap".into(),
        reason: "smart-background".into(), outcome: "escalated".into(),
        attempts: 2, escalations: 1, input_tokens: 10, output_tokens: 5,
        task_summary: "do the thing".into(),
    };
    let line = event_to_json_line(&ev); // the writer's serialization fn (extract if inline)
    assert!(line.contains("mur.routing"));
    assert!(line.contains("smart-background"));
    assert!(line.contains("\"outcome\":\"escalated\""));
}
```

(If the serialization is inlined in the writer loop, extract a testable `fn event_to_json_line(&Event) -> String` as part of this task — a pure move — so this test can call it.)

- [ ] **Step 2: Run to verify it fails** — `cargo test -p mur-agent-runtime routing_event_serializes` → FAIL.

- [ ] **Step 3: Implement**
1. Add the `Event::Routing { agent, task_id, intent, model_ref, reason, outcome, attempts, escalations, input_tokens, output_tokens, task_summary }` variant.
2. Add its writer-match arm producing a line with `"kind":"mur.routing"` + the fields (mirror the `Event::LlmCall` arm's `params` style; use a `METHOD_ROUTING = "mur.routing"` const).
3. In `FallbackLlmClient`: add `telemetry: Option<tokio::sync::mpsc::Sender<Event>>` field + a setter/constructor param; after the generate loop resolves (Ok or final Err), build the `Event::Routing` (intent → string, reason/outcome from the loop's final state, tokens from the winning `LlmResponse` or 0, task_summary = first user text of `req` truncated to `ROUTING_SUMMARY_MAX = 200`) and `let _ = tx.try_send(ev);` (best-effort — never await, never fail the turn).
4. In `build_provider_runner`/`client_builder`: pass `Some(telemetry.sender())` when constructing the routed/fallback client. The single-model path may pass `None` (no routing to record) or also record — record for parity is fine; keep it simple: only the `FallbackLlmClient` records.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p mur-agent-runtime routing_event fallback:: telemetry` then `cargo build -p mur-agent-runtime`. Expected: PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(telemetry): mur.routing outcome recording from FallbackLlmClient (best-effort, all intents)"`

---

### Task 6: Intent tagging at call sites + usage payload fields

**Files:** Modify `mur-agent-runtime/src/task_runner.rs` (generate sites), companion outbox generate, `Task.usage`/per-turn usage payload

**Interfaces:**
- Consumes: `RequestIntent` (T1). Produces: background call sites set `req.intent`; `Task.usage` (or the per-turn usage JSON) gains `model_ref: String` + `route_reason: String`.

- [ ] **Step 1: Tag the call sites** (behavior-preserving where Interactive)

Locate the generate call sites via `grep -n "\.generate(\|generate_stream(" mur-agent-runtime/src`. For each, set `req.intent` + `req.task_id` from context:
- Scheduled task execution path → `RequestIntent::Background(BackgroundKind::Scheduled)`.
- Companion outbox generation (`companion/outbox/generate.rs`) → `Background(Companion)`.
- Sleep-cycle / maintenance runtime-initiated generates → `Background(Maintenance)`.
- Chat / A2A `message/send` / fleet delegate → leave default `Interactive` (no change).
Set `req.task_id = context.task_id.clone()` where a task id is in scope (for telemetry correlation).

- [ ] **Step 2: Thread model_ref + route_reason into the usage payload**

The runtime already reports per-turn usage (`Task.usage` → Hub). Add `model_ref: String` + `route_reason: String` to that struct. Source them from what the `FallbackLlmClient` actually used: expose the winning candidate + reason from `generate` (e.g. store on a per-task field the client can report, or return via the response path). Simplest: the `Event::Routing` already has them; also stash the last `(model_ref, reason)` on the client keyed by `task_id` and have the runner read it when building usage. Wire so the Hub receives `model_ref`/`route_reason`.

- [ ] **Step 3: Verify** — `cargo build -p mur-agent-runtime` + `cargo test -p mur-agent-runtime`; a test asserting the scheduled path constructs `intent = Background(Scheduled)` (extract the request-building into a testable helper if needed). Confirm Interactive paths are unchanged (grep shows no `intent =` on chat/delegate paths). Expected: builds + green.

- [ ] **Step 4: Commit** — `git commit -m "feat(runtime): tag background generate sites + model_ref/route_reason in usage"`

---

## Hub UI (npm + visual QA — Phase-2 pattern)

### Task 7: Hub Settings — Smart toggle + cheap picker

**Files:** Modify `mur-hub-gui/ui/src/components/settings/modelSwitch.ts` (types), `ModelsSettings.tsx`; `mur-hub-gui/src-tauri/src/model_switch.rs` (validation).

**Interfaces:** Consumes Phase-2 `model_switch_get/set` (full-object; the new `smart` field rides along automatically). Produces UI for `smart.enabled` + `smart.cheap`.

- [ ] **Step 1: Extend the TS type + normalize** (`modelSwitch.ts`)

Add to `ModelSwitchView`: `smart: { enabled: boolean; cheap: string | null; max_escalations: number }`. Extend `normalizeMs` (Phase-2 Critical guard) to fill `smart` defaults (`enabled: raw.smart?.enabled ?? true`, `cheap: raw.smart?.cheap ?? null`, `max_escalations: raw.smart?.max_escalations ?? 1`). Add a vitest assertion that a payload without `smart` normalizes to `enabled:true, cheap:null`.

- [ ] **Step 2: Run helper test** — `cd mur-hub-gui/ui && npm test -- modelSwitch` → passes after normalize update.

- [ ] **Step 3: Render the Smart controls** in the MODEL SWITCHING section of `ModelsSettings.tsx`: a **Smart mode** checkbox bound to `ms.smart.enabled` (→ `saveMs({...ms, smart:{...ms.smart, enabled}})`), and a `ModelRefSelect` for `ms.smart.cheap` (allowEmpty; empty label = "auto (cheapest chat model)"), shown when enabled. Plain-language hint (i18n keys in BOTH `en.ts` + `zh-TW.ts`): "Background tasks run on a cost-saving model and escalate automatically if they fail."

- [ ] **Step 4: Rust validation** — in `model_switch.rs` `validate_refs`, also validate `next.smart.cheap` (when `Some`) against `models.yaml`. Add a test `set_validates_smart_cheap_ref`.

- [ ] **Step 5: Verify** — `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml validate_smart` + `cd mur-hub-gui/ui && npm test && npm run build`. Commit.

---

### Task 8: Hub chat — decision caption + one-click re-run

**Files:** Modify the Hub chat message component (grep `route_reason`/where agent replies render usage) + the A2A send path.

**Interfaces:** Consumes `Task.usage.model_ref`/`route_reason` (T6). Produces the caption + a re-run that sends `pin_model_ref`.

- [ ] **Step 1: Locate the chat reply renderer** — `grep -rn "usage\|model_ref\|cost_usd" mur-hub-gui/ui/src/components` to find where an agent reply shows metadata.

- [ ] **Step 2: Render the caption** — under each agent reply, a small caption `⚡ {model_ref} · {routeLabel(route_reason)}` (localized; `routeLabel` maps `smart-background`→"Smart (background)", `explicit`→model name only, etc.). Interactive replies show just the model.

- [ ] **Step 3: Re-run control** — when `route_reason` indicates a Smart downgrade, render "↑ re-run on smart model". On click, re-send the same user turn via the existing send path with `pin_model_ref` = the agent's primary model_ref in the message metadata. The runtime (T1/T3) threads `pin_model_ref` into `LlmRequest` → `candidates_for` returns `[pinned]`. The re-run's `mur.routing` event carries `outcome:"user_corrected"` (the runtime sets this when `pin_model_ref` is present AND the original was a Smart downgrade — thread a flag, or simpler: any `pin_model_ref` re-run records `user_corrected`).

- [ ] **Step 4: Verify** — `cd mur-hub-gui/ui && npm run build` (type-checks the new metadata field + props). Commit.

---

### Task 9: Hub build + visual QA (controller/operator gate)

**Not a code task.** Build the Hub `.app` (memory `gotcha_hub_local_app_build_recipe`: copy sidecars, `npx @tauri-apps/cli@2 build --debug --bundles app`, ad-hoc sign) and verify:
- Settings → Models: **Smart mode** toggle (on by default) + cheap picker; toggling persists to `~/.mur/config.yaml` `models.smart`.
- Run a **background** task (e.g. a scheduled task or companion nudge) → its reply caption shows the cheap model + "Smart (background)"; `~/.mur/agents/<name>/telemetry/<date>.jsonl` contains a `mur.routing` line.
- A Smart-downgraded reply shows "↑ re-run on smart model"; clicking it re-runs on the primary and a `user_corrected` routing event is recorded.
- An **interactive** chat reply is unchanged (normal model, no Smart caption, no crash).

---

## Self-Review

**Spec coverage:** RequestIntent/pin/task_id → T1; SmartConfig + auto-pick + per-agent → T2; candidate assembly (Background+Smart, pin) → T3; cascade InvalidResponse-only-under-Background+Smart + cap + Interactive-Fatal-unchanged → T4; mur.routing recording (all intents, best-effort, task_summary) → T5; intent tagging + usage fields → T6; Hub Smart settings → T7; chat caption + re-run + user_corrected → T8; build+QA → T9. ✓

**Placeholder scan:** T3 Step 3 and T6 Step 2 say "adapt to the existing body / read it first" with the exact additive block specified and the compile constraint stated — these are grounded (the surrounding Phase-1/2 code exists and must be read), not hand-waves. T5 notes extracting `event_to_json_line` if inlined (a concrete refactor). No TBD/TODO.

**Type consistency:** `RequestIntent`/`BackgroundKind` (T1) used in T3/T4/T6. `SmartConfig{enabled,cheap,max_escalations}` (T2) used in T3/T4/T7. `pick_cheap_model(reg, exclude)` (T2) used in T3. `Event::Routing{…}` fields (T5) match the spec's JSON + the T7/T8 `model_ref`/`route_reason` usage. `pin_model_ref` (T1) → T3 candidates + T8 re-run. `normalizeMs` (Phase-2) extended in T7. Consistent.

**Cross-phase safety:** the Interactive fatal boundary (Phase-1) is explicitly preserved and test-guarded in T4 (`interactive_invalid_response_stays_fatal` + the surviving Phase-1 `fatal_error_does_not_advance`). Additive `LlmRequest` fields guarded by T1's default test + `cargo build` catching non-spread literals.

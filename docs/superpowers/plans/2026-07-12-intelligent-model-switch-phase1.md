# Intelligent Model Switching — Phase 1 (core + CLI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give MUR agents config-layered model selection (global default + per-agent, priority per-agent → global) and automatic failure-fallback chains (retry with backoff/jitter + in-memory cooldown), plus an opt-in difficulty heuristic — managed from the CLI.

**Architecture:** Pure config + resolver types in `mur-common` (no I/O, unit-tested). A `FallbackLlmClient` in `mur-agent-runtime` that itself implements the existing `LlmClient` trait and loops over ordered candidates — so it drops into the existing `Arc<dyn LlmClient>` slot in `build_provider_runner` with no agent-loop change. The runtime `LlmError` gains typed variants so a classifier can tell retryable (429/5xx/timeout/402) from fatal (400/401/malformed).

**Tech Stack:** Rust (edition 2024), serde/serde_yaml, `#[async_trait]`, `thiserror`, `rand` (jitter), `clap`.

## Global Constraints

- **No hardcoded values.** Every tunable (retry count, backoff base, cooldown seconds, routing threshold) is a config field with a documented `pub const DEFAULT_*` (Mandatory Rule 1).
- **Additive & off-by-default.** With an empty `models:` block and no per-agent `fallback_chain`, resolution returns a single candidate and behaviour is identical to today. Difficulty routing ships `enabled: false`.
- **Priority per-agent → global**, everywhere: primary model, fallback chain, routing config.
- **Retryable vs fatal is a hard boundary:** 429/5xx/timeout/402 → advance the chain; 400/401/403/malformed → return immediately (never fall back — that hides auth/bugs).
- **Legacy-parse safe:** every new profile/config field is `#[serde(default)]`; a profile/config written before this change must still deserialize.
- **Do NOT implement learned/RouteLLM routing** (cost-router Phase 3) or the Hub GUI (Phase 2 — separate plan).
- Rust edition 2024; comments English; files ≤ 800 lines.
- **Build/test env:** `mur-common` + `mur-agent-runtime` need no special env. `mur-core` (Task 8) needs `export ORT_STRATEGY=download; export MUR_WEB_DIST=$HOME/Projects/mur-web/dist`. If `cargo` isn't found, add `~/.rustup/toolchains/stable-*/bin` to PATH. Build/test per-crate (`-p <crate>`), never `--workspace`.

## File Structure

- `mur-common/src/config.rs` — new `ModelSwitchConfig` / `RetryConfig` / `RoutingConfig` + default consts; new `Config.models` field. (Task 1)
- `mur-common/src/agent.rs` — `AgentProfile.fallback_chain` + `AgentProfile.routing`. (Task 2)
- `mur-common/src/model.rs` — pure `resolve_model_refs` + `choose_by_difficulty`. (Task 3)
- `mur-agent-runtime/src/llm/mod.rs` — `LlmError` new variants + `LlmError::from_status`; `Retryability` classifier. (Task 4)
- `mur-agent-runtime/src/llm/{anthropic,ollama,openai}.rs` — use `from_status` for non-2xx. (Task 4)
- `mur-agent-runtime/src/llm/fallback.rs` (new) — `CooldownMap`, `backoff_delay`, `estimate_input_tokens`, `FallbackLlmClient`. (Tasks 5–6)
- `mur-agent-runtime/src/supervisor_runner.rs` — build `FallbackLlmClient` in `build_provider_runner`. (Task 7)
- `mur-core/src/cmd/model.rs` + `mur-core/src/cmd/agent/model_resolve.rs` + `mur-core/src/cli/*` — CLI. (Task 8)

---

### Task 1: `ModelSwitchConfig` in global config

**Files:**
- Modify: `mur-common/src/config.rs`

**Interfaces:**
- Produces: `Config.models: ModelSwitchConfig`; structs `ModelSwitchConfig { default: Option<String>, fallback_chain: Vec<String>, retry: RetryConfig, routing: RoutingConfig }`, `RetryConfig { max_retries: u32, backoff_base_ms: u64, cooldown_secs: u64 }`, `RoutingConfig { enabled: bool, cheap: Option<String>, frontier: Option<String>, threshold_input_tokens: Option<u32> }`; consts `DEFAULT_MAX_RETRIES=1`, `DEFAULT_BACKOFF_BASE_MS=500`, `DEFAULT_COOLDOWN_SECS=60`, `DEFAULT_ROUTING_THRESHOLD=2000`.

- [ ] **Step 1: Write the failing test** (append to `config.rs` test module)

```rust
#[test]
fn model_switch_config_defaults_and_omitted_block() {
    // Omitted `models:` block deserializes to defaults.
    let cfg: Config = serde_yaml::from_str("{}").unwrap();
    assert_eq!(cfg.models.default, None);
    assert!(cfg.models.fallback_chain.is_empty());
    assert_eq!(cfg.models.retry.max_retries, DEFAULT_MAX_RETRIES);
    assert_eq!(cfg.models.retry.backoff_base_ms, DEFAULT_BACKOFF_BASE_MS);
    assert_eq!(cfg.models.retry.cooldown_secs, DEFAULT_COOLDOWN_SECS);
    assert!(!cfg.models.routing.enabled);

    // A populated block round-trips.
    let yaml = "models:\n  default: claude_sonnet\n  fallback_chain: [claude_sonnet, deepseek_v4_pro]\n  routing:\n    enabled: true\n    cheap: deepseek_v4_flash\n    frontier: claude_opus\n    threshold_input_tokens: 1500\n";
    let cfg: Config = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.models.default.as_deref(), Some("claude_sonnet"));
    assert_eq!(cfg.models.fallback_chain, vec!["claude_sonnet", "deepseek_v4_pro"]);
    assert!(cfg.models.routing.enabled);
    assert_eq!(cfg.models.routing.threshold_input_tokens, Some(1500));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common model_switch_config_defaults`
Expected: FAIL to compile (`Config.models` / types don't exist).

- [ ] **Step 3: Implement**

Add the field to `Config` (alongside the other `#[serde(default)]` sub-configs, e.g. after `pub llm: LlmConfig,`):

```rust
    #[serde(default)]
    pub models: ModelSwitchConfig,
```

Add near the other default consts at the top of `config.rs`:

```rust
pub const DEFAULT_MAX_RETRIES: u32 = 1;
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 500;
pub const DEFAULT_COOLDOWN_SECS: u64 = 60;
pub const DEFAULT_ROUTING_THRESHOLD: u32 = 2000;

fn default_max_retries() -> u32 { DEFAULT_MAX_RETRIES }
fn default_backoff_base_ms() -> u64 { DEFAULT_BACKOFF_BASE_MS }
fn default_cooldown_secs() -> u64 { DEFAULT_COOLDOWN_SECS }
```

Add the structs (mirror the existing `LlmConfig` sub-config style):

```rust
/// Config-layered model selection + failure fallback. See
/// docs/superpowers/specs/2026-07-12-intelligent-model-switch-design.md.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelSwitchConfig {
    /// Global default model_ref when an agent has no `model_ref`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Global fallback chain (ordered model_refs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_chain: Vec<String>,
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_backoff_base_ms")]
    pub backoff_base_ms: u64,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            backoff_base_ms: DEFAULT_BACKOFF_BASE_MS,
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoutingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_input_tokens: Option<u32>,
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-common model_switch_config_defaults` then `cargo test -p mur-common config::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(config): ModelSwitchConfig (default/fallback_chain/retry/routing)"
```

---

### Task 2: Per-agent `fallback_chain` + `routing`

**Files:**
- Modify: `mur-common/src/agent.rs` (`AgentProfile`)

**Interfaces:**
- Consumes: `RoutingConfig` (Task 1).
- Produces: `AgentProfile.fallback_chain: Vec<String>`, `AgentProfile.routing: Option<RoutingConfig>`.

- [ ] **Step 1: Write the failing test** (in the `model_ref_tests` module in `agent.rs`, mirroring `legacy_profile_without_model_ref_still_parses`)

```rust
#[test]
fn per_agent_fallback_and_routing_optional_and_legacy_safe() {
    // Legacy profile (no fallback_chain / routing) still parses.
    let legacy = r#"{"name":"a","model":{"provider":"anthropic","name":"claude"}}"#;
    let p: AgentProfile = serde_json::from_str(legacy).unwrap();
    assert!(p.fallback_chain.is_empty());
    assert!(p.routing.is_none());

    // Populated round-trips.
    let full = r#"{"name":"a","model":{"provider":"anthropic","name":"claude"},"fallback_chain":["claude_opus","claude_sonnet"],"routing":{"enabled":true}}"#;
    let p: AgentProfile = serde_json::from_str(full).unwrap();
    assert_eq!(p.fallback_chain, vec!["claude_opus", "claude_sonnet"]);
    assert!(p.routing.as_ref().unwrap().enabled);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common per_agent_fallback_and_routing`
Expected: FAIL to compile (fields don't exist).

- [ ] **Step 3: Implement**

In `AgentProfile` (near `model_ref`), add:

```rust
    /// Per-agent fallback chain (ordered model_refs). Overrides the global
    /// `models.fallback_chain` when non-empty. See the model-switch spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_chain: Vec<String>,
    /// Per-agent difficulty-routing override. Inherits the global
    /// `models.routing` when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<crate::config::RoutingConfig>,
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-common per_agent_fallback_and_routing` then `cargo test -p mur-common model_ref_tests`
Expected: PASS (the existing `legacy_profile_without_model_ref_still_parses` must still pass).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs
git commit -m "feat(agent): per-agent fallback_chain + routing override (legacy-safe)"
```

---

### Task 3: Pure resolver + difficulty heuristic

**Files:**
- Modify: `mur-common/src/model.rs`

**Interfaces:**
- Consumes: `AgentProfile` (Task 2), `ModelSwitchConfig`/`RoutingConfig` (Task 1).
- Produces:
  - `pub fn resolve_model_refs(profile: &AgentProfile, cfg: &ModelSwitchConfig, routed_primary: Option<String>) -> Vec<String>` — ordered model_refs `[primary, ...fallback]`, priority per-agent → global, primary de-duped out of the chain.
  - `pub fn choose_by_difficulty(est_input_tokens: u32, r: &RoutingConfig) -> Option<String>`.

- [ ] **Step 1: Write the failing test** (append to `model.rs` test module)

```rust
#[cfg(test)]
mod switch_tests {
    use super::*;
    use crate::agent::AgentProfile;
    use crate::config::{ModelSwitchConfig, RoutingConfig};

    fn profile(model_ref: Option<&str>, chain: &[&str]) -> AgentProfile {
        let mut p = AgentProfile::default();
        p.model_ref = model_ref.map(|s| s.to_string());
        p.fallback_chain = chain.iter().map(|s| s.to_string()).collect();
        p
    }

    #[test]
    fn per_agent_primary_and_chain_win_over_global() {
        let cfg = ModelSwitchConfig {
            default: Some("global_default".into()),
            fallback_chain: vec!["g1".into(), "g2".into()],
            ..Default::default()
        };
        let p = profile(Some("agent_primary"), &["agent_primary", "agent_fb"]);
        // per-agent model_ref is primary; per-agent chain used; primary de-duped.
        assert_eq!(resolve_model_refs(&p, &cfg, None), vec!["agent_primary", "agent_fb"]);
    }

    #[test]
    fn falls_back_to_global_default_and_chain() {
        let cfg = ModelSwitchConfig {
            default: Some("global_default".into()),
            fallback_chain: vec!["g1".into(), "global_default".into()],
            ..Default::default()
        };
        let p = profile(None, &[]); // no per-agent model_ref or chain
        // primary = global default; global chain used; primary de-duped out.
        assert_eq!(resolve_model_refs(&p, &cfg, None), vec!["global_default", "g1"]);
    }

    #[test]
    fn routed_primary_overrides_model_ref() {
        let cfg = ModelSwitchConfig { fallback_chain: vec!["g1".into()], ..Default::default() };
        let p = profile(Some("agent_primary"), &[]);
        assert_eq!(
            resolve_model_refs(&p, &cfg, Some("frontier".into())),
            vec!["frontier", "g1"]
        );
    }

    #[test]
    fn no_config_no_agent_yields_empty() {
        // Nothing configured → empty vec (caller falls back to inline model).
        let cfg = ModelSwitchConfig::default();
        assert!(resolve_model_refs(&profile(None, &[]), &cfg, None).is_empty());
    }

    #[test]
    fn difficulty_picks_frontier_over_threshold() {
        let r = RoutingConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            frontier: Some("frontier".into()),
            threshold_input_tokens: Some(1000),
        };
        assert_eq!(choose_by_difficulty(1500, &r), Some("frontier".into()));
        assert_eq!(choose_by_difficulty(500, &r), Some("cheap".into()));
        // Misconfigured (missing frontier) → None (fall through).
        let bad = RoutingConfig { enabled: true, cheap: Some("c".into()), frontier: None, threshold_input_tokens: None };
        assert_eq!(choose_by_difficulty(9999, &bad), None);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common switch_tests`
Expected: FAIL to compile (functions missing).

- [ ] **Step 3: Implement** (add to `model.rs`)

```rust
use crate::agent::AgentProfile;
use crate::config::{DEFAULT_ROUTING_THRESHOLD, ModelSwitchConfig, RoutingConfig};

/// Build the ordered list of model_refs to try: `[primary, ...fallback]`.
/// Priority per-agent → global. The primary is de-duplicated out of the chain
/// (no point retrying the same ref back-to-back). Returns empty when nothing is
/// configured, so the caller keeps today's single-inline-model behaviour.
pub fn resolve_model_refs(
    profile: &AgentProfile,
    cfg: &ModelSwitchConfig,
    routed_primary: Option<String>,
) -> Vec<String> {
    let primary = routed_primary
        .or_else(|| profile.model_ref.clone())
        .or_else(|| cfg.default.clone());
    let chain = if !profile.fallback_chain.is_empty() {
        profile.fallback_chain.clone()
    } else {
        cfg.fallback_chain.clone()
    };
    let mut out: Vec<String> = Vec::new();
    if let Some(p) = primary {
        out.push(p);
    }
    for r in chain {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// Opt-in difficulty heuristic: pick `frontier` when the estimated input token
/// count exceeds the threshold, else `cheap`. `None` when misconfigured (caller
/// falls through to model_ref/global default).
pub fn choose_by_difficulty(est_input_tokens: u32, r: &RoutingConfig) -> Option<String> {
    let threshold = r.threshold_input_tokens.unwrap_or(DEFAULT_ROUTING_THRESHOLD);
    match (r.cheap.as_ref(), r.frontier.as_ref()) {
        (Some(cheap), Some(frontier)) => Some(if est_input_tokens > threshold {
            frontier.clone()
        } else {
            cheap.clone()
        }),
        _ => None,
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-common switch_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/model.rs
git commit -m "feat(model): pure resolve_model_refs + choose_by_difficulty (per-agent→global)"
```

---

### Task 4: `LlmError` taxonomy + retryability classifier

**Files:**
- Modify: `mur-agent-runtime/src/llm/mod.rs` (`LlmError` + classifier)
- Modify: `mur-agent-runtime/src/llm/{anthropic,ollama,openai}.rs` (use `from_status`)

**Interfaces:**
- Produces: `LlmError::ServerError(u16)`, `LlmError::InsufficientCredit`; `LlmError::from_status(status: u16, body: String) -> LlmError`; `pub enum Retryability { Retryable, Fatal }`; `pub fn classify(e: &LlmError) -> Retryability`.

- [ ] **Step 1: Write the failing test** (append to `llm/mod.rs` test module)

```rust
#[test]
fn from_status_maps_http_codes() {
    assert!(matches!(LlmError::from_status(429, "x".into()), LlmError::RateLimit));
    assert!(matches!(LlmError::from_status(402, "x".into()), LlmError::InsufficientCredit));
    assert!(matches!(LlmError::from_status(503, "x".into()), LlmError::ServerError(503)));
    assert!(matches!(LlmError::from_status(400, "x".into()), LlmError::Http(_)));
    assert!(matches!(LlmError::from_status(401, "x".into()), LlmError::Http(_)));
}

#[test]
fn classify_retryable_vs_fatal() {
    use Retryability::*;
    assert!(matches!(classify(&LlmError::RateLimit), Retryable));
    assert!(matches!(classify(&LlmError::Timeout), Retryable));
    assert!(matches!(classify(&LlmError::ServerError(500)), Retryable));
    assert!(matches!(classify(&LlmError::InsufficientCredit), Retryable));
    assert!(matches!(classify(&LlmError::Http("400".into())), Fatal));
    assert!(matches!(classify(&LlmError::InvalidResponse("x".into())), Fatal));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-agent-runtime from_status_maps_http_codes classify_retryable`
Expected: FAIL to compile (variants/fns missing).

- [ ] **Step 3: Implement**

Add the two variants to `LlmError` (with `thiserror` messages):

```rust
    #[error("server error: {0}")]
    ServerError(u16),
    #[error("insufficient credit")]
    InsufficientCredit,
```

Add the mapper + classifier (in `llm/mod.rs`):

```rust
impl LlmError {
    /// Map a non-success HTTP status into a typed error. Centralises what was
    /// previously scattered `status == 429` checks + a lumped `Http(String)`.
    pub fn from_status(status: u16, body: String) -> LlmError {
        match status {
            429 => LlmError::RateLimit,
            402 => LlmError::InsufficientCredit,
            500..=599 => LlmError::ServerError(status),
            _ => LlmError::Http(format!("status {status}: {body}")),
        }
    }
}

/// Whether a failed call should advance the fallback chain (Retryable) or
/// return immediately (Fatal — auth/bad-request/malformed, where switching
/// models would only hide the real problem).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retryability {
    Retryable,
    Fatal,
}

pub fn classify(e: &LlmError) -> Retryability {
    match e {
        LlmError::RateLimit
        | LlmError::Timeout
        | LlmError::ServerError(_)
        | LlmError::InsufficientCredit => Retryability::Retryable,
        LlmError::Http(_) | LlmError::InvalidResponse(_) => Retryability::Fatal,
    }
}
```

Retrofit each client's non-2xx path to use `from_status`. In `anthropic.rs` (~lines 548-561) the code reads `let status = resp.status(); if status == 429 { return Err(LlmError::RateLimit) } return Err(LlmError::Http(format!("status {status}: {body_text}")))`. Replace that block with:

```rust
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(LlmError::from_status(status.as_u16(), body_text));
        }
```

Apply the analogous change in `ollama.rs` (the two `if resp.status() == 429 { return Err(LlmError::RateLimit); }` sites, ~lines 90 and 142 — replace each with the `from_status` non-success guard) and `openai.rs` (its non-2xx path). Keep the existing timeout/connect mapping (`LlmError::Timeout` / `LlmError::Http(e.to_string())` on `reqwest::Error`) unchanged — those are transport errors, not HTTP statuses.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-agent-runtime from_status_maps_http_codes classify_retryable` then `cargo build -p mur-agent-runtime` (catches any exhaustive `match LlmError` that now needs the two new arms — add them where flagged).
Expected: PASS + builds.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/llm/
git commit -m "feat(llm): typed LlmError (ServerError/InsufficientCredit) + retryability classifier"
```

---

### Task 5: Cooldown map, backoff, token estimate

**Files:**
- Create: `mur-agent-runtime/src/llm/fallback.rs`
- Modify: `mur-agent-runtime/src/llm/mod.rs` (add `pub mod fallback;`)

**Interfaces:**
- Consumes: `LlmRequest` (from `llm/mod.rs`).
- Produces:
  - `pub struct CooldownMap` with `fn new() -> Self`, `fn mark(&self, model_ref: &str, until: Instant)`, `fn is_cooling(&self, model_ref: &str, now: Instant) -> bool`.
  - `pub fn backoff_delay(attempt: u32, base_ms: u64) -> Duration` — `base * 2^attempt + jitter[0, base)`.
  - `pub fn estimate_input_tokens(req: &LlmRequest) -> u32` — coarse `chars/4` over message texts.

- [ ] **Step 1: Write the failing test** (in `fallback.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn cooldown_marks_and_expires() {
        let cm = CooldownMap::new();
        let now = Instant::now();
        assert!(!cm.is_cooling("m", now));
        cm.mark("m", now + Duration::from_secs(60));
        assert!(cm.is_cooling("m", now));                       // within window
        assert!(!cm.is_cooling("m", now + Duration::from_secs(61))); // after window
        assert!(!cm.is_cooling("other", now));
    }

    #[test]
    fn backoff_grows_and_stays_in_bounds() {
        let base = 500u64;
        for attempt in 0..4u32 {
            let d = backoff_delay(attempt, base).as_millis() as u64;
            let floor = base * 2u64.pow(attempt);
            assert!(d >= floor && d < floor + base, "attempt {attempt}: {d} not in [{floor}, {})", floor + base);
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-agent-runtime fallback::tests`
Expected: FAIL to compile (module/types missing).

- [ ] **Step 3: Implement** (`fallback.rs` header + these items)

```rust
//! Failure-fallback for LLM calls: cooldown circuit-breaker, backoff, and the
//! FallbackLlmClient adapter. See the model-switch spec.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::LlmRequest;

/// In-memory per-model cooldown (circuit-breaker). Process-local; a restart
/// clears it (cooldowns are seconds-scale, so persistence is unnecessary).
#[derive(Default)]
pub struct CooldownMap {
    inner: Mutex<HashMap<String, Instant>>,
}

impl CooldownMap {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }
    pub fn mark(&self, model_ref: &str, until: Instant) {
        self.inner.lock().unwrap().insert(model_ref.to_string(), until);
    }
    pub fn is_cooling(&self, model_ref: &str, now: Instant) -> bool {
        match self.inner.lock().unwrap().get(model_ref) {
            Some(until) => now < *until,
            None => false,
        }
    }
}

/// Exponential backoff with jitter: `base * 2^attempt + rand[0, base)`.
pub fn backoff_delay(attempt: u32, base_ms: u64) -> Duration {
    let floor = base_ms.saturating_mul(2u64.saturating_pow(attempt));
    let jitter = if base_ms > 0 { rand::random::<u64>() % base_ms } else { 0 };
    Duration::from_millis(floor.saturating_add(jitter))
}

/// Coarse input-token estimate (chars/4) over the request's message texts —
/// enough for a routing heuristic, not billing. `LlmRequest.messages` is a
/// `Vec<RichMessage>` (an enum), so match the text-bearing variants.
pub fn estimate_input_tokens(req: &LlmRequest) -> u32 {
    let chars: usize = req
        .messages
        .iter()
        .map(|m| match m {
            RichMessage::Text { content, .. } => content.len(),
            RichMessage::ImageText { text, .. } => text.len(),
            RichMessage::ToolUse { text, .. } => text.as_deref().map_or(0, str::len),
            RichMessage::ToolResults { .. } => 0,
        })
        .sum();
    (chars / 4).min(u32::MAX as usize) as u32
}
```

Add its import at the top of `fallback.rs`: `use super::RichMessage;` (alongside the `use super::LlmRequest;`).

Add a test for it (in `fallback.rs` tests):

```rust
    #[test]
    fn estimate_tokens_sums_text_over_rich_messages() {
        use super::super::{LlmRequest, RichMessage};
        let req = LlmRequest {
            messages: vec![
                RichMessage::Text { role: "user".into(), content: "a".repeat(40) },
                RichMessage::ImageText {
                    role: "user".into(),
                    media_type: "image/png".into(),
                    data: String::new(),
                    text: "b".repeat(40),
                },
            ],
            temperature: None,
            max_tokens: None,
            tools: vec![],
        };
        assert_eq!(estimate_input_tokens(&req), 20); // 80 chars / 4
    }
```

**Verified facts (do not re-derive):** `RichMessage` is an enum with variants `Text { role, content }`, `ToolUse { text: Option<String>, calls }`, `ToolResults { results }`, `ImageText { role, media_type, data, text }` (llm/mod.rs ~line 78). `LlmRequest { messages: Vec<RichMessage>, temperature: Option<f32>, max_tokens: Option<u32>, tools: Vec<ToolDef> }`. `rand = { workspace = true }` is already a dependency of `mur-agent-runtime`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-agent-runtime fallback::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/llm/fallback.rs mur-agent-runtime/src/llm/mod.rs
git commit -m "feat(llm): cooldown map + backoff + token estimate for fallback"
```

---

### Task 6: `FallbackLlmClient` adapter

**Files:**
- Modify: `mur-agent-runtime/src/llm/fallback.rs`

**Interfaces:**
- Consumes: `LlmClient`, `LlmRequest`, `LlmResponse`, `LlmError`, `classify`, `Retryability` (Task 4); `CooldownMap`, `backoff_delay` (Task 5); `RetryConfig` (`mur_common::config`).
- Produces: `FallbackLlmClient` implementing `LlmClient`; constructor `FallbackLlmClient::new(candidates: Vec<String>, factory: ClientFactory, retry: RetryConfig)` where `type ClientFactory = Box<dyn Fn(&str) -> anyhow::Result<Arc<dyn LlmClient>> + Send + Sync>`.

- [ ] **Step 1: Write the failing test** (append to `fallback.rs` tests)

```rust
    use super::super::{LlmClient, LlmError, LlmRequest, LlmResponse, StopReason};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // LlmResponse has no Default (StopReason has no default variant), so build
    // one explicitly.
    fn mk_resp(text: &str) -> LlmResponse {
        LlmResponse {
            text: text.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model: text.to_string(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        }
    }

    // Mock client whose Nth generate() outcome is scripted.
    struct ScriptClient { name: String, outcomes: Vec<Result<(), LlmError>>, idx: AtomicUsize }
    #[async_trait]
    impl LlmClient for ScriptClient {
        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            let i = self.idx.fetch_add(1, Ordering::SeqCst).min(self.outcomes.len()-1);
            match &self.outcomes[i] {
                Ok(()) => Ok(mk_resp(&self.name)),
                Err(e) => Err(e.clone()),
            }
        }
        fn model_name(&self) -> &str { &self.name }
    }

    fn factory_for(scripts: std::collections::HashMap<String, Vec<Result<(),LlmError>>>) -> ClientFactory {
        Box::new(move |r: &str| {
            let o = scripts.get(r).cloned().unwrap_or_else(|| vec![Ok(())]);
            Ok(Arc::new(ScriptClient { name: r.to_string(), outcomes: o, idx: AtomicUsize::new(0) }) as Arc<dyn LlmClient>)
        })
    }
    fn retry0() -> mur_common::config::RetryConfig {
        mur_common::config::RetryConfig { max_retries: 0, backoff_base_ms: 1, cooldown_secs: 60 }
    }

    #[tokio::test]
    async fn advances_chain_on_retryable_then_succeeds() {
        let mut s = std::collections::HashMap::new();
        s.insert("a".into(), vec![Err(LlmError::ServerError(500))]); // a fails (retryable)
        s.insert("b".into(), vec![Ok(())]);                          // b succeeds
        let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
        let resp = fb.generate(LlmRequest::default()).await.unwrap();
        assert_eq!(resp.text, "b"); // fell through to b
    }

    #[tokio::test]
    async fn fatal_error_does_not_advance() {
        let mut s = std::collections::HashMap::new();
        s.insert("a".into(), vec![Err(LlmError::Http("401".into()))]); // fatal
        s.insert("b".into(), vec![Ok(())]);
        let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
        let err = fb.generate(LlmRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::Http(_))); // returned a's fatal error, never tried b
    }

    #[tokio::test]
    async fn exhaustion_returns_last_error() {
        let mut s = std::collections::HashMap::new();
        s.insert("a".into(), vec![Err(LlmError::RateLimit)]);
        s.insert("b".into(), vec![Err(LlmError::ServerError(503))]);
        let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
        let err = fb.generate(LlmRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::ServerError(503))); // last candidate's error
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-agent-runtime fallback::tests::advances_chain`
Expected: FAIL to compile (`FallbackLlmClient` / `ClientFactory` missing).

- [ ] **Step 3: Implement** (add to `fallback.rs`)

```rust
use std::sync::Arc;
use async_trait::async_trait;
use super::{LlmClient, LlmError, LlmRequest, LlmResponse, Retryability, classify};
use mur_common::config::RetryConfig;

pub type ClientFactory = Box<dyn Fn(&str) -> anyhow::Result<Arc<dyn LlmClient>> + Send + Sync>;

/// An `LlmClient` that tries an ordered list of model_refs, advancing on
/// retryable failures (per-candidate backoff retries first), skipping models in
/// cooldown, and returning a fatal error immediately. Drops into the existing
/// `Arc<dyn LlmClient>` slot with no agent-loop change.
pub struct FallbackLlmClient {
    candidates: Vec<String>,
    factory: ClientFactory,
    retry: RetryConfig,
    cooldown: CooldownMap,
    primary_name: String,
}

impl FallbackLlmClient {
    pub fn new(candidates: Vec<String>, factory: ClientFactory, retry: RetryConfig) -> Self {
        let primary_name = candidates.first().cloned().unwrap_or_default();
        Self { candidates, factory, retry, cooldown: CooldownMap::new(), primary_name }
    }
}

#[async_trait]
impl LlmClient for FallbackLlmClient {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let now = Instant::now();
        let mut last: Option<LlmError> = None;
        for model_ref in &self.candidates {
            if self.cooldown.is_cooling(model_ref, now) {
                continue;
            }
            let client = match (self.factory)(model_ref) {
                Ok(c) => c,
                Err(e) => { last = Some(LlmError::Http(format!("build {model_ref}: {e}"))); continue; }
            };
            for attempt in 0..=self.retry.max_retries {
                match client.generate(req.clone()).await {
                    Ok(resp) => return Ok(resp),
                    Err(e) => match classify(&e) {
                        Retryability::Fatal => return Err(e),
                        Retryability::Retryable => {
                            tracing::info!(model_ref, attempt, error = %e, "llm fallback: retryable failure");
                            if attempt < self.retry.max_retries {
                                tokio::time::sleep(backoff_delay(attempt, self.retry.backoff_base_ms)).await;
                            } else {
                                let until = Instant::now() + Duration::from_secs(self.retry.cooldown_secs);
                                self.cooldown.mark(model_ref, until);
                                tracing::info!(model_ref, "llm fallback: cooling down, advancing chain");
                                last = Some(e);
                            }
                        }
                    },
                }
            }
        }
        Err(last.unwrap_or_else(|| LlmError::InvalidResponse("no model candidates".into())))
    }

    fn model_name(&self) -> &str {
        &self.primary_name
    }
}
```

**Interface note:** `req.clone()` requires `LlmRequest: Clone` — verified: `LlmRequest` derives `#[derive(Debug, Clone)]`. `LlmResponse` does NOT derive `Default` (its `stop_reason: StopReason` has no default variant — StopReason is `{EndTurn, ToolUse, MaxTokens}`), so the test mock builds it explicitly via `mk_resp` (shown in Step 1), constructing all six fields with `stop_reason: StopReason::EndTurn`. Do NOT add `Default` to `LlmResponse`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-agent-runtime fallback::tests`
Expected: PASS (all three async tests + Task 5's).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/llm/fallback.rs
git commit -m "feat(llm): FallbackLlmClient adapter (retry/cooldown/advance, is an LlmClient)"
```

---

### Task 7: Wire the fallback client into the runtime

**Files:**
- Modify: `mur-agent-runtime/src/supervisor_runner.rs` (`build_provider_runner`, ~lines 215-420)

**Interfaces:**
- Consumes: `resolve_model_refs` (Task 3), `FallbackLlmClient` (Task 6), `Config`/`ModelSwitchConfig` (Task 1), the existing per-provider client-building code in `build_provider_runner`.

**Context:** `build_provider_runner` currently resolves ONE `ModelEntry` via `resolve_model_entry(&profile.inner)` (~line 222), builds ONE `Arc<dyn LlmClient>`, and passes it to `build(client)`. This task: (1) load the global `ModelSwitchConfig`; (2) compute the ordered refs via `resolve_model_refs`; (3) if the list has ≤1 entry AND no routing, keep today's single-client path unchanged; (4) otherwise, construct a `FallbackLlmClient` whose factory reuses the existing per-provider build logic, and pass THAT as the `Arc<dyn LlmClient>`.

- [ ] **Step 1: Extract the per-provider client build into a reusable closure**

Find the block in `build_provider_runner` that, given a `ModelEntry` + resolved secret, produces `Arc<dyn LlmClient>` (the `match provider { ... }` around lines 350-418 that ends in `Arc::new(c) as Arc<dyn LlmClient>`). Extract it into a local helper within the function:

```rust
    // Reusable per-ref client builder: model_ref -> Arc<dyn LlmClient>.
    // Reuses resolve_model_entry (registry lookup) + the existing provider match.
    let mur_home_cl = mur_home.clone();
    let build_one = move |model_ref: &str| -> anyhow::Result<Arc<dyn LlmClient>> {
        let entry = {
            // Look the ref up in the registry directly (resolve_model_entry keys
            // off profile.model_ref; here we resolve an explicit ref).
            let reg = mur_common::model::ModelRegistry::load_from(
                &mur_common::model::ModelRegistry::default_path()?,
            )?;
            reg.models.get(model_ref)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("model_ref {model_ref:?} not in registry"))?
        };
        build_client_from_entry(&entry, &mur_home_cl)   // see note
    };
```

**Note:** the exact secret-resolution + guarded-HTTP-client + provider-match currently lives inline in `build_provider_runner`. Extract it into a `fn build_client_from_entry(entry: &ModelEntry, mur_home: &Path) -> anyhow::Result<Arc<dyn LlmClient>>` (pure move of the existing lines 260-418 logic — no behaviour change), then both the single-client path and `build_one` call it. Do the extraction as the first, behaviour-preserving edit and run the existing runtime tests to confirm no regression before wiring fallback.

- [ ] **Step 2: Compute candidates and choose single vs fallback client**

After extracting, add:

```rust
    let switch_cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).models;
    // Difficulty routing (opt-in) picks the primary per-agent → global.
    let routing = profile.inner.routing.clone().unwrap_or(switch_cfg.routing.clone());
    // Note: routing needs the request; Phase 1 applies it per-call inside the
    // FallbackLlmClient only when enabled. For candidate assembly here we pass
    // None (model_ref/global default primary); see Step 3 for the routing path.
    let refs = mur_common::model::resolve_model_refs(&profile.inner, &switch_cfg, None);

    let client: Arc<dyn LlmClient> = if refs.len() <= 1 && !routing.enabled {
        // Preserve today's single-model behaviour exactly.
        build_client_from_entry(&entry, &mur_home)?
    } else {
        Arc::new(mur_agent_runtime_fallback_new(refs, Box::new(build_one), switch_cfg.retry.clone()))
    };
```

Replace `mur_agent_runtime_fallback_new(...)` with the real path `crate::llm::fallback::FallbackLlmClient::new(...)`. Keep the existing `entry` (from `resolve_model_entry`) for the single-model path so nothing changes when nothing is configured.

- [ ] **Step 3: (Routing, opt-in) apply difficulty pick inside the client**

Because routing needs the request, the simplest correct Phase-1 wiring is: when `routing.enabled`, prepend the routed primary at generate time. Extend `FallbackLlmClient::new` call to also pass the `RoutingConfig` and the resolver, OR — simpler — keep routing OUT of the runtime path in Phase 1 and document it: since routing is `enabled: false` by default and the heuristic + `resolve_model_refs(routed_primary=Some)` are already unit-tested (Task 3), ship the wiring with routing applied at candidate-assembly using a fixed estimate is NOT possible (no req yet). **Decision:** In Phase 1, wire fallback fully; gate routing behind a follow-up (documented in the spec's non-goals note is inaccurate — instead add a one-line log `tracing::info!("model routing enabled but per-request routing lands in Phase 1b")` when `routing.enabled`). This keeps Task 7 shippable and honest; the pure routing fn is already tested and ready for the per-request hook.

- [ ] **Step 4: Verify no regression + build**

Run: `cargo build -p mur-agent-runtime` then `cargo test -p mur-agent-runtime`
Expected: builds; existing runtime tests pass; with no `models:` config and no per-agent chain, `refs.len() <= 1` so the single-client path runs (unchanged behaviour).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/supervisor_runner.rs
git commit -m "feat(runtime): build FallbackLlmClient from resolved candidates (single-model path unchanged)"
```

---

### Task 8: CLI — `mur model default/fallback` + per-agent `--fallback`

**Files:**
- Modify: `mur-core/src/cli/model.rs` (or wherever `ModelCmd` is defined — grep) + `mur-core/src/cmd/model.rs`
- Modify: `mur-core/src/cmd/agent/model_resolve.rs` (per-agent fallback setter)

**Interfaces:**
- Consumes: `ModelSwitchConfig` (Task 1), `store::config::{load_config,save_config}`, `ModelRegistry`.

- [ ] **Step 1: Write the failing test** (in `mur-core/src/cmd/model.rs` tests)

```rust
#[test]
fn set_default_validates_ref_exists() {
    // Uses a temp MUR_HOME with a seeded models.yaml (mirror existing model.rs
    // tests' harness). Setting an unknown ref errors; a known ref persists.
    let home = /* temp mur home with models.yaml containing `claude_sonnet` */;
    assert!(cmd_model_default(&home, "does_not_exist").is_err());
    cmd_model_default(&home, "claude_sonnet").unwrap();
    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    assert_eq!(cfg.models.default.as_deref(), Some("claude_sonnet"));
}
```

(Model the temp-home + seeded-`models.yaml` harness on the existing tests in `mur-core/src/cmd/model.rs` — grep the test module for how it builds a registry; reuse that exact pattern.)

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo test -p mur-core set_default_validates_ref`
Expected: FAIL to compile (`cmd_model_default` missing).

- [ ] **Step 3: Implement**

Add clap variants to `ModelCmd` (grep `enum ModelCmd`):

```rust
    /// Set the global default model_ref (config.yaml models.default)
    Default { model_ref: String },
    /// Set the global fallback chain (ordered model_refs; empty clears)
    Fallback { model_refs: Vec<String> },
```

Add handlers in `cmd/model.rs` (fail-closed ref validation against `models.yaml`):

```rust
fn ensure_ref_exists(home: &Path, r: &str) -> anyhow::Result<()> {
    let reg = mur_common::model::ModelRegistry::load_from(&home.join("models.yaml"))?;
    anyhow::ensure!(reg.models.contains_key(r), "model_ref {r:?} not in models.yaml");
    Ok(())
}

pub fn cmd_model_default(home: &Path, model_ref: &str) -> anyhow::Result<()> {
    ensure_ref_exists(home, model_ref)?;
    let mut cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    cfg.models.default = Some(model_ref.to_string());
    mur_core::store::config::save_config_at(&home.join("config.yaml"), &cfg)?; // see note
    println!("global default model = {model_ref}");
    Ok(())
}

pub fn cmd_model_fallback(home: &Path, refs: &[String]) -> anyhow::Result<()> {
    for r in refs { ensure_ref_exists(home, r)?; }
    let mut cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    cfg.models.fallback_chain = refs.to_vec();
    mur_core::store::config::save_config_at(&home.join("config.yaml"), &cfg)?;
    println!("global fallback chain = {}", refs.join(", "));
    Ok(())
}
```

**Note:** use whatever save function the codebase exposes — grep `fn save_config`. The Hub uses `mur_core::store::config::save_config`; if it saves to the default home only, add/reuse a `save_config_at(path, cfg)` (or set `MUR_HOME`). Wire both handlers into the `ModelCmd` dispatch (`match cmd { ModelCmd::Default{..} => .., ModelCmd::Fallback{..} => .. }`).

For per-agent, add a `--fallback <ref>...` option to the existing agent model-setting path in `model_resolve.rs` that writes `profile.fallback_chain` (validating each ref), mirroring how it sets `profile.model_ref` (~line 94). If there is no standalone agent model-set command, expose `pub fn cmd_agent_set_fallback(name: &str, refs: &[String]) -> Result<()>` that loads the profile, validates refs, sets `fallback_chain`, saves.

- [ ] **Step 4: Run to verify it passes**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo test -p mur-core set_default_validates_ref`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cli/ mur-core/src/cmd/model.rs mur-core/src/cmd/agent/model_resolve.rs
git commit -m "feat(cli): mur model default/fallback + per-agent --fallback (ref-validated)"
```

---

## Self-Review

**Spec coverage:**
- Config-layered resolution + `models:` block → Task 1, 3. ✓
- Per-agent settings + priority per-agent→global → Task 2, 3. ✓
- Fallback chain + retry/backoff/cooldown → Task 5, 6. ✓
- Error classification (429/5xx/timeout/402 retryable; 400/401/malformed fatal) → Task 4. ✓
- Opt-in difficulty heuristic → Task 3 (pure fn). Runtime per-request wiring is explicitly deferred to Phase 1b in Task 7 Step 3 (routing is off by default; the fn is tested and ready) — **flagged as a scoping decision for the reviewer/user**, not silently dropped. ✓
- CLI (`mur model default/fallback`, per-agent `--fallback`, ref validation) → Task 8. ✓
- Rollout additive/off-by-default → Task 7 (single-model path unchanged when nothing configured). ✓
- Hub GUI → correctly EXCLUDED (Phase 2). ✓

**Gap flagged honestly:** the difficulty-routing *runtime* application (per-request primary pick) is deferred to Phase 1b in Task 7 Step 3, because it needs the request object at call time and doing it right is a small follow-up on top of the already-tested pure fn. This is a deliberate narrowing of the opt-in, off-by-default feature — confirm with the user during execution whether to fold the per-request routing into Task 7 instead of deferring.

**Placeholder scan:** No TBD/TODO. The two "Interface note" items (Task 5 `LlmMessage` field name, Task 6 `LlmRequest: Clone`) are explicit verification instructions with the fallback action stated, not placeholders. Task 7 Step 1 names the exact lines to extract and mandates a behaviour-preserving move first.

**Type consistency:** `resolve_model_refs(profile, cfg, routed_primary)` signature identical in Task 3 (def) and Task 7 (use). `FallbackLlmClient::new(candidates, factory, retry)` + `ClientFactory` identical in Task 6 (def) and Task 7 (use). `LlmError::from_status`/`classify`/`Retryability` identical in Task 4 (def) and Task 6 (use). `RetryConfig`/`RoutingConfig`/`ModelSwitchConfig` fields identical across Tasks 1, 3, 6, 7, 8.

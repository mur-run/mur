# Cloud LLM Backend P1 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add Anthropic cloud LLM support to the `mur conversations compact` pipeline as the second canary call site after P0's `ask::rewriter`. Lands the `BackendConfig` schema in `mur-common`, an `AnthropicBackend` raw-HTTP impl, a composable `RetryingBackend` decorator, and `mur conversations doctor` cloud-provider checks. **Backward compat is non-negotiable**: every existing `~/.mur/config.yaml` must keep working byte-identically when no per-stage `backend` field is set.

**Architecture:** Composition via decorators — `factory::build(spec) -> Arc<RetryingBackend<inner>>` where `inner` is `OllamaBackend` or `AnthropicBackend`. Per-stage config overrides via `Option<BackendConfig>` fields on `CompactConfig` and `AskConfig`; legacy fields synthesize a `BackendConfig` when the override is `None`. P0's `BackendSpec` (in `mur-core/src/conversations/backend/factory.rs`) is replaced by the new `mur-common::config::BackendConfig`.

**Tech Stack:** Rust 2024 · `reqwest` (already a dep) · `tokio` · `tracing` · `anyhow` for application errors · `thiserror` for typed `BackendError` (extended) · `wiremock = "0.6"` (already a dev-dep in `mur-core`) for HTTP mocking · raw HTTP to Anthropic (no Rust SDK exists for Anthropic API).

**Spec:** `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` — §4 (trait), §5.2 (AnthropicBackend), §6 (BackendConfig), §7 (per-stage routing), §8.1 (retry envelope), §8.3 (doctor). P2 (streaming on the trait), P3 (prompt caching + cost telemetry), P4 (delete `mur-core/src/llm.rs`) are out of scope.

**Out of scope for P1** — explicitly do not implement:
- Streaming on `AnthropicBackend` (`generate_stream` keeps the P0 `bail!("not wired in P0")` stub for `OllamaBackend`; `AnthropicBackend` gets the same stub) — P2
- Migrating `ask::generate`, `ask::abstractive::compress_hit`, `summarize::abstractive`, `summarize::rollup` — P2/P3
- Prompt caching wiring (`cache_system` / `cache_user_prefix` hints stay defaulted-to-false; AnthropicBackend ignores them) — P3
- Cost telemetry / `cost-report` command — P3
- Migrating `learn`/`extract_llm` — P4
- Keychain integration — P3 (per spec §10; P1 stays env-var-only)
- Defaulting to `claude-opus-4-7` — defaults stay at `claude-haiku-4-5` per spec §7 + §13

**Plan deviation flagged from spec:** spec §10 says `secret_ref` field on `BackendConfig` lands in P3. P1 ships only `api_key_env: Option<String>`. The `BackendConfig` Rust struct does NOT include a `secret_ref` field yet — adding it later via `#[serde(default)]` is backward-compatible.

---

## Task 0: Verify foundation + dependencies (no commit)

**Files:** none modified.

**Step 1: Confirm P0 is on main**

Run:
```bash
git log --oneline | grep 79e4b72
```
Expected: `79e4b72 refactor(conversations): introduce ChatBackend trait (P0 of cloud-LLM rollout) (#80)`. If missing, **STOP** — the P1 plan assumes the trait exists.

**Step 2: Verify dev-dep `wiremock 0.6` is available**

Run: `grep '^wiremock' /Users/david/Projects/mur/mur-core/Cargo.toml`
Expected: `wiremock = "0.6"`. If absent, add to `[dev-dependencies]` in `mur-core/Cargo.toml`.

**Step 3: Read the master spec end-to-end**

Use the Read tool on `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md`. Internalize §4–§8 and §12.

**Step 4: Read the existing backend module**

Read `mur-core/src/conversations/backend/{mod.rs, mock.rs, ollama.rs, factory.rs}` end-to-end. Note `ChatBackend` trait shape, `BackendError` variants, `ChatRequest` / `ChatResponse` / `Usage` / `ChatChunk` / `ChatStream` types. Note that `BackendSpec` in `factory.rs` is what we're replacing.

**Step 5: Read the retry shape source**

`sed -n '215,260p' /Users/david/Projects/mur/mur-core/src/extract_llm.rs` — the existing 3-attempt loop. P1 replaces the string-based transient detection (`err_str.contains("529")`) with typed dispatch on `BackendError` variants.

**Step 6: No commit** — context-loading only.

---

## Task 1: Add `BackendConfig` to `mur-common`

**Files:**
- Modify: `mur-common/src/config.rs` (add `BackendConfig` struct near other config types)
- Test: same file (append to `#[cfg(test)] mod tests` if exists, else create one)

**Step 1: Write the failing tests**

Append to `mur-common/src/config.rs`:

```rust
/// Backend selection for a single chat-completion call site.
///
/// Per spec §6. Used by `CompactConfig` (per-stage) and `AskConfig`
/// (per-stage) to override the legacy Ollama-only path. None of the
/// `Option` fields are required; resolution falls back to provider
/// defaults (Ollama: http://localhost:11434, Anthropic: https://api.anthropic.com).
///
/// Stays in mur-common (not mur-core) because it is pure data and
/// will be reused by mur-agent-runtime in a future phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BackendConfig {
    /// "ollama" | "anthropic". Defaults to "ollama" for backward compat.
    pub provider: String,
    /// Model name as the provider sees it ("claude-haiku-4-5", "qwen3:14b", …).
    pub model: String,
    /// Provider endpoint. None = provider default
    /// (ollama: http://localhost:11434, anthropic: https://api.anthropic.com).
    pub endpoint: Option<String>,
    /// Env var holding the API key. None = no auth (ollama).
    pub api_key_env: Option<String>,
    /// Per-call timeout in seconds. None = 120s.
    pub timeout_secs: Option<u64>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            endpoint: None,
            api_key_env: None,
            timeout_secs: None,
        }
    }
}
```

Then add tests at the bottom of the file (or in the existing `#[cfg(test)] mod tests` block if one exists — check first):

```rust
#[cfg(test)]
mod backend_config_tests {
    use super::*;

    #[test]
    fn default_is_ollama_qwen3() {
        let cfg = BackendConfig::default();
        assert_eq!(cfg.provider, "ollama");
        assert_eq!(cfg.model, "qwen3:14b");
        assert_eq!(cfg.endpoint, None);
        assert_eq!(cfg.api_key_env, None);
        assert_eq!(cfg.timeout_secs, None);
    }

    #[test]
    fn deserializes_anthropic_full() {
        let yaml = "\
provider: anthropic
model: claude-haiku-4-5
api_key_env: ANTHROPIC_API_KEY
timeout_secs: 60
";
        let cfg: BackendConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.model, "claude-haiku-4-5");
        assert_eq!(cfg.api_key_env, Some("ANTHROPIC_API_KEY".into()));
        assert_eq!(cfg.timeout_secs, Some(60));
        assert_eq!(cfg.endpoint, None);
    }

    #[test]
    fn deserializes_partial_fills_defaults() {
        let yaml = "provider: anthropic\nmodel: claude-sonnet-4-6\n";
        let cfg: BackendConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.model, "claude-sonnet-4-6");
        assert_eq!(cfg.api_key_env, None);
        assert_eq!(cfg.timeout_secs, None);
    }

    #[test]
    fn round_trips_through_yaml() {
        let original = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: Some("https://api.anthropic.com".into()),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            timeout_secs: Some(60),
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        let parsed: BackendConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, original);
    }
}
```

**Step 2: Run tests to confirm they fail**

Run: `cargo test -p mur-common backend_config_tests 2>&1 | tail -10`

Expected: FAIL — `BackendConfig` not yet defined.

**Step 3: The implementation is the struct + Default impl shown above**

(Step 1 already wrote them in-place.)

**Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-common backend_config_tests 2>&1 | tail -10`

Expected: PASS — 4 tests.

**Step 5: Lint and format**

Run:
```bash
cargo fmt -p mur-common && cargo fmt --check -p mur-common
cargo clippy -p mur-common -- -D warnings
```
Expected: clean.

**Step 6: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "$(cat <<'EOF'
feat(common): add BackendConfig for per-stage LLM provider selection

Backend-agnostic data type used by mur-core conversations to override
the legacy Ollama-only call sites. Default = ollama+qwen3:14b for
backward compat. All Option fields nullable so existing config files
keep deserializing byte-identically when no override is set.

Lives in mur-common (not mur-core) per crate charter — pure data,
no logic, will be reused by mur-agent-runtime in a future phase.

Refs spec docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md §6.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Per-stage `Option<BackendConfig>` overrides on `CompactConfig` + `AskConfig`

**Files:**
- Modify: `mur-common/src/config.rs:319-345` (`AskConfig`)
- Modify: `mur-common/src/config.rs:476-498` (`CompactConfig`)
- Test: same file

**Step 1: Write the failing backward-compat tests first**

Append to `mur-common/src/config.rs`:

```rust
#[cfg(test)]
mod per_stage_backend_tests {
    use super::*;

    #[test]
    fn legacy_compact_config_has_no_per_stage_overrides() {
        let yaml = "\
extractive_model: qwen3:14b
abstractive_model: qwen3:14b
ollama_endpoint: http://localhost:11434
";
        let cfg: CompactConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.extractive_backend.is_none());
        assert!(cfg.abstractive_backend.is_none());
        // Legacy fields preserved
        assert_eq!(cfg.extractive_model, "qwen3:14b");
        assert_eq!(cfg.abstractive_model, "qwen3:14b");
        assert_eq!(cfg.ollama_endpoint, "http://localhost:11434");
    }

    #[test]
    fn legacy_ask_config_has_no_per_stage_overrides() {
        let yaml = "model: qwen3:14b\nollama_endpoint: http://localhost:11434\n";
        let cfg: AskConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.backend.is_none());
        assert!(cfg.rewriter_backend.is_none());
        assert_eq!(cfg.model, "qwen3:14b");
    }

    #[test]
    fn compact_extractive_backend_override_parses() {
        let yaml = "\
extractive_backend:
  provider: anthropic
  model: claude-haiku-4-5
  api_key_env: ANTHROPIC_API_KEY
abstractive_model: qwen3:14b
";
        let cfg: CompactConfig = serde_yaml::from_str(yaml).unwrap();
        let extractive = cfg.extractive_backend.as_ref().expect("override should parse");
        assert_eq!(extractive.provider, "anthropic");
        assert_eq!(extractive.model, "claude-haiku-4-5");
        assert!(cfg.abstractive_backend.is_none()); // not overridden
    }

    #[test]
    fn ask_rewriter_backend_can_override_to_local_while_answer_is_cloud() {
        let yaml = "\
backend:
  provider: anthropic
  model: claude-sonnet-4-6
  api_key_env: ANTHROPIC_API_KEY
rewriter_backend:
  provider: ollama
  model: llama3.2:3b
";
        let cfg: AskConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.backend.as_ref().unwrap().provider, "anthropic");
        assert_eq!(cfg.rewriter_backend.as_ref().unwrap().provider, "ollama");
    }

    #[test]
    fn synthesize_legacy_to_backend_config_for_compact_extractive() {
        let yaml = "extractive_model: qwen3:14b\nollama_endpoint: http://192.168.1.10:11434\n";
        let cfg: CompactConfig = serde_yaml::from_str(yaml).unwrap();
        let synth = cfg.synthesize_extractive_backend();
        assert_eq!(synth.provider, "ollama");
        assert_eq!(synth.model, "qwen3:14b");
        assert_eq!(synth.endpoint.as_deref(), Some("http://192.168.1.10:11434"));
    }

    #[test]
    fn synthesize_legacy_to_backend_config_for_ask() {
        let yaml = "model: qwen3:14b\nollama_endpoint: http://localhost:11434\n";
        let cfg: AskConfig = serde_yaml::from_str(yaml).unwrap();
        let synth = cfg.synthesize_backend();
        assert_eq!(synth.provider, "ollama");
        assert_eq!(synth.model, "qwen3:14b");
        assert_eq!(synth.endpoint.as_deref(), Some("http://localhost:11434"));
    }
}
```

**Step 2: Run tests to confirm they fail**

Run: `cargo test -p mur-common per_stage_backend_tests 2>&1 | tail -15`

Expected: FAIL — fields and methods don't exist yet.

**Step 3: Add the new fields + helper methods**

Edit `CompactConfig` (around line 476). Add at the END of the struct (so existing field order stays stable):

```rust
    /// Per-stage backend override for extractive summarization.
    /// None = synthesize from legacy `extractive_model` + `ollama_endpoint`.
    #[serde(default)]
    pub extractive_backend: Option<BackendConfig>,
    /// Per-stage backend override for abstractive summarization.
    /// None = synthesize from legacy `abstractive_model` + `ollama_endpoint`.
    #[serde(default)]
    pub abstractive_backend: Option<BackendConfig>,
```

Edit `AskConfig` (around line 319). Add at the END of the struct:

```rust
    /// Per-stage backend override for the answer-generation model.
    /// None = synthesize from legacy `model` + `ollama_endpoint`.
    #[serde(default)]
    pub backend: Option<BackendConfig>,
    /// Per-stage backend override for the query rewriter.
    /// None = synthesize from legacy `model` + `ollama_endpoint`
    /// (rewriter shares the answer model in the legacy path).
    #[serde(default)]
    pub rewriter_backend: Option<BackendConfig>,
```

Then add helper methods. After the `CompactConfig` struct definition:

```rust
impl CompactConfig {
    /// Returns the effective backend for the extractive stage.
    /// Per-stage override wins; otherwise synthesize from legacy fields.
    pub fn synthesize_extractive_backend(&self) -> BackendConfig {
        self.extractive_backend.clone().unwrap_or_else(|| BackendConfig {
            provider: "ollama".into(),
            model: self.extractive_model.clone(),
            endpoint: Some(self.ollama_endpoint.clone()),
            api_key_env: None,
            timeout_secs: None,
        })
    }

    /// Returns the effective backend for the abstractive stage.
    pub fn synthesize_abstractive_backend(&self) -> BackendConfig {
        self.abstractive_backend.clone().unwrap_or_else(|| BackendConfig {
            provider: "ollama".into(),
            model: self.abstractive_model.clone(),
            endpoint: Some(self.ollama_endpoint.clone()),
            api_key_env: None,
            timeout_secs: None,
        })
    }
}
```

After the `AskConfig` struct definition:

```rust
impl AskConfig {
    /// Returns the effective backend for the answer-generation model.
    pub fn synthesize_backend(&self) -> BackendConfig {
        self.backend.clone().unwrap_or_else(|| BackendConfig {
            provider: "ollama".into(),
            model: self.model.clone(),
            endpoint: Some(self.ollama_endpoint.clone()),
            api_key_env: None,
            timeout_secs: None,
        })
    }

    /// Returns the effective backend for the query rewriter.
    /// Falls back to the answer backend if no rewriter override.
    pub fn synthesize_rewriter_backend(&self) -> BackendConfig {
        self.rewriter_backend.clone().unwrap_or_else(|| self.synthesize_backend())
    }
}
```

**Step 4: Run tests to verify pass**

Run: `cargo test -p mur-common per_stage_backend_tests 2>&1 | tail -15`

Expected: PASS — 6 tests.

Also re-run all `mur-common` tests to confirm no regression in existing config tests:

Run: `cargo test -p mur-common 2>&1 | tail -10`

Expected: all pass.

**Step 5: Lint and format**

Run:
```bash
cargo fmt -p mur-common && cargo fmt --check -p mur-common
cargo clippy -p mur-common -- -D warnings
```

**Step 6: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "$(cat <<'EOF'
feat(common): per-stage BackendConfig overrides on CompactConfig + AskConfig

CompactConfig gains optional `extractive_backend` / `abstractive_backend`.
AskConfig gains optional `backend` / `rewriter_backend`.
Each has a `synthesize_*_backend` helper that returns the per-stage
override if Some, else synthesizes a BackendConfig from the legacy
ollama_endpoint + per-stage model fields. Existing configs unchanged.

6 new backward-compat tests verify legacy YAML still deserializes with
overrides as None and synthesize methods produce the expected ollama
backend.

Refs spec §6 (per-stage routing schema) and §7 (rationale).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Replace P0 `BackendSpec` with `BackendConfig` in `factory.rs`

**Files:**
- Modify: `mur-core/src/conversations/backend/factory.rs` (delete `BackendSpec` struct + impl, replace with `mur_common::config::BackendConfig`)
- Modify: `mur-core/src/cmd/conversations_cmd.rs` (call site that uses `BackendSpec::ollama(...)` → use `BackendConfig` directly)

**Step 1: Read the current factory.rs and find the call site**

```bash
cat /Users/david/Projects/mur/mur-core/src/conversations/backend/factory.rs
grep -n "BackendSpec" /Users/david/Projects/mur/mur-core/src/cmd/conversations_cmd.rs
```

You should see:
- `factory.rs` defines `BackendSpec` and `BackendSpec::ollama(...)`
- `cmd_ask` constructs `BackendSpec::ollama(&ask_cfg.ollama_endpoint, ask_cfg.rewriter_timeout_secs as u64)`

**Step 2: Update the failing tests in factory.rs**

The existing 4 factory tests use `BackendSpec::ollama(...)`. Replace each occurrence with the equivalent `BackendConfig` literal. The tests' assertions don't change — they still verify provider name dispatch.

Specifically, in `factory.rs`'s `#[cfg(test)] mod tests`, change every:
```rust
let spec = BackendSpec::ollama("http://localhost:11434", 5);
```
to:
```rust
let spec = BackendConfig {
    provider: "ollama".into(),
    model: "qwen3:14b".into(),
    endpoint: Some("http://localhost:11434".into()),
    api_key_env: None,
    timeout_secs: Some(5),
};
```

And the `unsupported_provider_errors` test — change `BackendSpec { ... }` literal similarly.

**Step 3: Replace the `factory.rs` definition body**

Replace the entire content of `mur-core/src/conversations/backend/factory.rs` with:

```rust
//! ChatBackend factory. Selects backend from BackendConfig (mur-common
//! schema). See spec §5.4.

#![allow(dead_code)] // wired into more call sites across P1.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use mur_common::config::BackendConfig;

use super::{ChatBackend, mock::MockBackend, ollama::OllamaBackend};

/// Build a backend from BackendConfig. Honors MUR_LLM_MOCK / MUR_OLLAMA_MOCK
/// env vars: when either is set, returns MockBackend regardless of cfg.
///
/// AnthropicBackend wiring lands in Task 5; until then "anthropic" provider
/// returns an error. Task 6 wraps the result in RetryingBackend.
pub fn build(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
    if std::env::var("MUR_LLM_MOCK").is_ok() || std::env::var("MUR_OLLAMA_MOCK").is_ok() {
        tracing::debug!(provider = %cfg.provider, "MUR_LLM_MOCK active — using MockBackend");
        return Ok(Arc::new(MockBackend::new()));
    }
    match cfg.provider.as_str() {
        "ollama" => {
            let endpoint = cfg.endpoint.as_deref().unwrap_or("http://localhost:11434");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Ok(Arc::new(OllamaBackend::new(endpoint, timeout)))
        }
        "anthropic" => bail!("anthropic backend not yet wired (lands in P1 Task 5)"),
        other => bail!("unsupported provider: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ollama_cfg(endpoint: &str, timeout_secs: u64) -> BackendConfig {
        BackendConfig {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            endpoint: Some(endpoint.into()),
            api_key_env: None,
            timeout_secs: Some(timeout_secs),
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mock_env_var_forces_mock_backend() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_LLM_MOCK", "1") };
        let cfg = ollama_cfg("http://localhost:11434", 5);
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "mock");
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn legacy_mur_ollama_mock_env_var_also_forces_mock() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let cfg = ollama_cfg("http://localhost:11434", 5);
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "mock");
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn ollama_provider_returns_ollama_backend() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let cfg = ollama_cfg("http://127.0.0.1:1", 1);
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "ollama");
    }

    #[test]
    fn anthropic_provider_unwired_in_task_3() {
        // Will become a real test in Task 5 when AnthropicBackend lands.
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("not yet wired"));
    }

    #[test]
    fn unsupported_provider_errors() {
        let cfg = BackendConfig {
            provider: "openai".into(),
            model: "gpt-4".into(),
            endpoint: None,
            api_key_env: None,
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("unsupported"));
    }
}
```

**Step 4: Update the call site in `cmd_ask`**

In `mur-core/src/cmd/conversations_cmd.rs`, find the rewriter setup (currently uses `BackendSpec::ollama(&ask_cfg.ollama_endpoint, ask_cfg.rewriter_timeout_secs as u64)`). Replace with:

```rust
    let mut rewriter_cfg = ask_cfg.synthesize_rewriter_backend();
    rewriter_cfg.timeout_secs = Some(ask_cfg.rewriter_timeout_secs as u64);
    let rewriter_backend = crate::conversations::backend::factory::build(&rewriter_cfg)?;
```

(The rewriter override carries a separate `rewriter_timeout_secs` from `AskConfig` because the rewriter has tighter latency budget — see comment block in cmd_ask. Override the synthesized config's timeout with the per-stage value.)

**Step 5: Run all backend + cmd tests**

```bash
cargo test -p mur-core --lib conversations::backend -- --test-threads=1 2>&1 | tail -15
cargo test -p mur-core --lib conversations::ask::rewriter -- --test-threads=1 2>&1 | tail -10
```

Expected: PASS for all.

**Step 6: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 7: Commit**

```bash
git add mur-core/src/conversations/backend/factory.rs mur-core/src/cmd/conversations_cmd.rs
git commit -m "$(cat <<'EOF'
refactor(backend): replace P0 BackendSpec with BackendConfig from mur-common

P0 shipped a minimal BackendSpec local to mur-core/src/conversations/backend/factory.rs.
P1 promotes the schema to mur-common::config::BackendConfig and wires
the factory to it. Adds an "anthropic" arm that errors with "not yet
wired" until Task 5 implements AnthropicBackend.

cmd_ask's rewriter setup now uses ask_cfg.synthesize_rewriter_backend()
to honor per-stage overrides, with rewriter_timeout_secs applied as the
timeout override (tighter than the answer model's budget).

Refs spec §5.4 (factory) and §6 (BackendConfig).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `RetryingBackend` decorator

**Files:**
- Create: `mur-core/src/conversations/backend/retry.rs`
- Modify: `mur-core/src/conversations/backend/mod.rs` (add `pub mod retry;`)

**Step 1: Write the failing tests**

Create `mur-core/src/conversations/backend/retry.rs`:

```rust
//! ChatBackend decorator that adds retry-with-exponential-backoff.
//!
//! Lifts the retry shape from mur-core/src/extract_llm.rs:215-260 but
//! dispatches on typed BackendError variants instead of string matching.
//!
//! Composable: factory::build wraps Anthropic + Ollama backends, but
//! tests can build MockBackend without retries (or wrap it manually).
//!
//! See spec §8.1.

#![allow(dead_code)] // wired into factory in Task 5.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::Stream;

use super::{BackendError, ChatBackend, ChatChunk, ChatRequest, ChatResponse};

/// Retry policy. P1 uses fixed defaults; future phases may make it configurable.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum total attempts (including the first). Default 3 = 1 try + 2 retries.
    pub max_attempts: u32,
    /// Base backoff in seconds — actual sleep = base * attempt.
    /// (Linear backoff for P1; exponential is overkill for our 3-attempt window.)
    pub base_backoff_secs: u64,
    /// Cap on retry-after honoring (for RateLimited).
    pub max_retry_after_secs: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff_secs: 2,
            max_retry_after_secs: 30,
        }
    }
}

pub struct RetryingBackend {
    inner: Arc<dyn ChatBackend>,
    policy: RetryPolicy,
}

impl RetryingBackend {
    pub fn new(inner: Arc<dyn ChatBackend>, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }

    /// Convenience: wrap with default policy.
    pub fn with_default_policy(inner: Arc<dyn ChatBackend>) -> Self {
        Self::new(inner, RetryPolicy::default())
    }

    /// Returns Some(sleep_duration) if we should retry, None if not.
    /// Splits the policy decision out so tests can assert it directly.
    fn should_retry(err: &anyhow::Error, attempt: u32, policy: &RetryPolicy) -> Option<Duration> {
        if attempt + 1 >= policy.max_attempts {
            return None;
        }
        let typed = err.downcast_ref::<BackendError>()?;
        let base = Duration::from_secs(policy.base_backoff_secs * (attempt + 1) as u64);
        match typed {
            BackendError::Timeout { .. } => Some(base),
            BackendError::ServerError { status, .. } if (500..=599).contains(status) => Some(base),
            BackendError::RateLimited { retry_after_secs, .. } => {
                let after = retry_after_secs
                    .map(|s| s.min(policy.max_retry_after_secs))
                    .unwrap_or(policy.base_backoff_secs);
                Some(Duration::from_secs(after))
            }
            // Non-retryable: Unauthorized, ModelNotFound, BadResponse, Network
            _ => None,
        }
    }
}

#[async_trait]
impl ChatBackend for RetryingBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let mut attempt: u32 = 0;
        loop {
            // ChatRequest borrows; clone the borrowed-fields-only owned form.
            let req_clone = ChatRequest {
                model: req.model,
                system: req.system,
                user: req.user,
                max_tokens: req.max_tokens,
                temperature: req.temperature,
                stop: req.stop.clone(),
                cache_system: req.cache_system,
                cache_user_prefix: req.cache_user_prefix,
            };
            match self.inner.generate(req_clone).await {
                Ok(resp) => return Ok(resp),
                Err(e) => match Self::should_retry(&e, attempt, &self.policy) {
                    Some(delay) => {
                        tracing::warn!(
                            provider = self.inner.provider_name(),
                            attempt = attempt + 1,
                            max_attempts = self.policy.max_attempts,
                            delay_secs = delay.as_secs(),
                            "backend transient error: {e:#}, retrying"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    None => return Err(e),
                },
            }
        }
    }

    async fn generate_stream(
        &self,
        req: ChatRequest<'_>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        // P1: streaming is not used by any wired call site. Pass through.
        // P2 will revisit retry semantics for streamed responses (mid-stream
        // retry is a different beast — likely just propagate the error).
        self.inner.generate_stream(req).await
    }

    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    fn supports_caching(&self) -> bool {
        self.inner.supports_caching()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::backend::{ChatChunk, ChatStream, Usage};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// Test backend that fails N times then returns success. Uses an
    /// atomic counter so retries are deterministic regardless of policy.
    struct FailNTimes {
        fail_n: u32,
        attempts: Arc<AtomicU32>,
        err_factory: fn() -> BackendError,
    }

    impl FailNTimes {
        fn new(fail_n: u32, err_factory: fn() -> BackendError) -> Self {
            Self {
                fail_n,
                attempts: Arc::new(AtomicU32::new(0)),
                err_factory,
            }
        }
    }

    #[async_trait]
    impl ChatBackend for FailNTimes {
        async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
            let n = self.attempts.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_n {
                return Err((self.err_factory)().into());
            }
            Ok(ChatResponse {
                text: "ok".into(),
                usage: Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    provider: "test",
                    model: req.model.into(),
                },
            })
        }

        async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
            anyhow::bail!("not used in retry tests")
        }

        fn provider_name(&self) -> &'static str { "test" }
    }

    fn req<'a>() -> ChatRequest<'a> {
        ChatRequest {
            model: "x", system: None, user: "p",
            max_tokens: 1, temperature: None, stop: vec![],
            cache_system: false, cache_user_prefix: None,
        }
    }

    fn fast_policy() -> RetryPolicy {
        // Sub-second backoff so tests don't actually wait.
        RetryPolicy { max_attempts: 3, base_backoff_secs: 0, max_retry_after_secs: 0 }
    }

    #[tokio::test]
    async fn retries_on_500_then_succeeds() {
        let inner = Arc::new(FailNTimes::new(2, || BackendError::ServerError {
            provider: "test", status: 500,
        }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let resp = backend.generate(req()).await.unwrap();
        assert_eq!(resp.text, "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // 1 + 2 retries
    }

    #[tokio::test]
    async fn retries_on_timeout() {
        let inner = Arc::new(FailNTimes::new(1, || BackendError::Timeout {
            provider: "test", seconds: 30,
        }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let _ = backend.generate(req()).await.unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2); // 1 + 1 retry
    }

    #[tokio::test]
    async fn does_not_retry_on_unauthorized() {
        let inner = Arc::new(FailNTimes::new(99, || BackendError::Unauthorized { provider: "test" }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let r = backend.generate(req()).await;
        assert!(r.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1); // No retries
    }

    #[tokio::test]
    async fn does_not_retry_on_model_not_found() {
        let inner = Arc::new(FailNTimes::new(99, || BackendError::ModelNotFound {
            provider: "test", model: "fake".into(),
        }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let _ = backend.generate(req()).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let inner = Arc::new(FailNTimes::new(99, || BackendError::ServerError {
            provider: "test", status: 503,
        }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let r = backend.generate(req()).await;
        assert!(r.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // max_attempts = 3
    }

    #[tokio::test]
    async fn rate_limited_honors_retry_after_capped() {
        let inner = Arc::new(FailNTimes::new(1, || BackendError::RateLimited {
            provider: "test", retry_after_secs: Some(99),
        }));
        let attempts = inner.attempts.clone();
        // Cap at 0s so the test doesn't actually wait, but verify the dispatch path.
        let policy = RetryPolicy { max_attempts: 3, base_backoff_secs: 0, max_retry_after_secs: 0 };
        let backend = RetryingBackend::new(inner, policy);
        let _ = backend.generate(req()).await.unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
```

Append to `mur-core/src/conversations/backend/mod.rs`:

```rust
pub mod retry;
```

**Step 2: Run tests to confirm they fail**

Run: `cargo test -p mur-core --lib conversations::backend::retry 2>&1 | tail -15`

Expected: FAIL — `retry` module + `RetryingBackend` not yet defined.

(Step 1 already wrote them; this is the TDD check that the test file compiles against the new API.)

**Step 3: Run tests to verify they pass**

Run: `cargo test -p mur-core --lib conversations::backend::retry 2>&1 | tail -15`

Expected: PASS — 6 tests.

**Step 4: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 5: Commit**

```bash
git add mur-core/src/conversations/backend/retry.rs mur-core/src/conversations/backend/mod.rs
git commit -m "$(cat <<'EOF'
feat(backend): add RetryingBackend decorator with typed dispatch

Composable wrapper around ChatBackend that retries on
BackendError::{Timeout, ServerError(500..=599), RateLimited} and
gives up immediately on Unauthorized, ModelNotFound, BadResponse,
Network.

Default policy: 3 attempts, base_backoff_secs=2 (linear: 2s, 4s),
max_retry_after_secs=30 (caps RateLimited.retry_after_secs honoring).

Streaming pass-through with no retry (mid-stream retry is a separate
problem deferred to P2).

Tests use a FailNTimes test backend with atomic counters and
sub-second backoff so they run in milliseconds.

Lifts the shape from mur-core/src/extract_llm.rs:215-260 but uses
typed BackendError dispatch instead of string matching on err.to_string().

Refs spec §8.1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `AnthropicBackend` (non-streaming) with wiremock tests

**Files:**
- Create: `mur-core/src/conversations/backend/anthropic.rs`
- Modify: `mur-core/src/conversations/backend/mod.rs` (add `pub mod anthropic;`)

**Step 1: Write the failing tests using wiremock**

Create `mur-core/src/conversations/backend/anthropic.rs`:

```rust
//! Anthropic Claude API backend. Raw HTTP via reqwest — no Rust SDK
//! exists for Anthropic. Non-streaming only in P1; streaming lands in P2.
//!
//! See spec §5.2.

#![allow(dead_code)] // wired by factory + compact.extractive in Tasks 6 & 7.

use std::pin::Pin;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use super::{BackendError, ChatBackend, ChatChunk, ChatRequest, ChatResponse, Usage};

const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct AnthropicBackend {
    endpoint: String,
    api_key: String,
    timeout: Duration,
    http: reqwest::Client,
}

impl AnthropicBackend {
    /// Construct from explicit api_key + endpoint. Pulls api_key from
    /// the env var named in BackendConfig.api_key_env at the factory
    /// boundary; this constructor takes the resolved key.
    pub fn new(endpoint: &str, api_key: &str, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client build");
        Self {
            endpoint: endpoint.trim_end_matches('/').into(),
            api_key: api_key.into(),
            timeout,
            http,
        }
    }
}

// ── Wire types ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<ApiMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop_sequences: Vec<String>,
    /// Always send `{type: "disabled"}` on Opus 4.6+ so we don't pay
    /// for implicit adaptive thinking. Older models accept it as a no-op.
    thinking: ApiThinking,
}

#[derive(Debug, Serialize)]
struct ApiMessage<'a> {
    role: &'a str, // "user"
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct ApiThinking {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    content: Vec<ApiContentBlock>,
    usage: ApiUsage,
    #[allow(dead_code)] // future telemetry
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    error: ApiErrorBody,
}

#[derive(Debug, Default, Deserialize)]
struct ApiErrorBody {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: String,
}

// ── Trait impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl ChatBackend for AnthropicBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let url = format!("{}/v1/messages", self.endpoint);

        // Sampling param removal on Opus 4.7 per claude-api skill.
        let temperature = if req.model.starts_with("claude-opus-4-7") {
            if req.temperature.is_some() {
                tracing::debug!(
                    model = req.model,
                    "dropping temperature for Opus 4.7 (sampling params 400 on this model)"
                );
            }
            None
        } else {
            req.temperature
        };

        let body = ApiRequest {
            model: req.model,
            max_tokens: if req.max_tokens == 0 { DEFAULT_MAX_TOKENS } else { req.max_tokens },
            system: req.system,
            messages: vec![ApiMessage { role: "user", content: req.user }],
            temperature,
            stop_sequences: req.stop.clone(),
            thinking: ApiThinking { kind: "disabled" },
        };

        let resp = self.http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|source| BackendError::Network { provider: "anthropic", source })?;

        let status = resp.status();
        if !status.is_success() {
            let raw_body = resp.text().await.unwrap_or_default();
            return Err(map_error(status, &raw_body, req.model));
        }

        let parsed: ApiResponse = resp.json().await.map_err(|e| {
            BackendError::BadResponse {
                provider: "anthropic",
                message: format!("json parse: {e}"),
            }
        })?;

        // Concatenate all text blocks; ignore non-text variants.
        let text = parsed.content
            .iter()
            .filter(|b| b.kind == "text")
            .map(|b| b.text.as_str())
            .collect::<String>();

        Ok(ChatResponse {
            text,
            usage: Usage {
                input_tokens: parsed.usage.input_tokens,
                output_tokens: parsed.usage.output_tokens,
                cache_creation_input_tokens: parsed.usage.cache_creation_input_tokens,
                cache_read_input_tokens: parsed.usage.cache_read_input_tokens,
                provider: "anthropic",
                model: req.model.into(),
            },
        })
    }

    async fn generate_stream(
        &self,
        _req: ChatRequest<'_>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        anyhow::bail!("AnthropicBackend::generate_stream lands in P2")
    }

    fn provider_name(&self) -> &'static str { "anthropic" }

    fn supports_caching(&self) -> bool {
        // P3 wiring; cache_system / cache_user_prefix hints are silently
        // ignored in P1.
        false
    }
}

/// Map an HTTP error response to the appropriate BackendError variant.
fn map_error(status: reqwest::StatusCode, body: &str, model: &str) -> anyhow::Error {
    let parsed: Option<ApiError> = serde_json::from_str(body).ok();
    let typed = match status.as_u16() {
        401 => BackendError::Unauthorized { provider: "anthropic" },
        404 => BackendError::ModelNotFound { provider: "anthropic", model: model.into() },
        429 => BackendError::RateLimited {
            provider: "anthropic",
            retry_after_secs: None, // wiremock can't easily set Retry-After; fine for now
        },
        s @ 500..=599 => BackendError::ServerError { provider: "anthropic", status: s },
        _ => BackendError::BadResponse {
            provider: "anthropic",
            message: parsed
                .map(|p| format!("{}: {}", p.error.kind, p.error.message))
                .unwrap_or_else(|| format!("status {status}: {body}")),
        },
    };
    typed.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn req<'a>(model: &'a str, user: &'a str) -> ChatRequest<'a> {
        ChatRequest {
            model,
            system: None,
            user,
            max_tokens: 16,
            temperature: Some(0.5),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        }
    }

    #[tokio::test]
    async fn provider_name_is_anthropic() {
        let b = AnthropicBackend::new("http://127.0.0.1:1", "k", Duration::from_millis(100));
        assert_eq!(b.provider_name(), "anthropic");
        assert!(!b.supports_caching());
    }

    #[tokio::test]
    async fn happy_path_returns_text_and_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_x",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "hello world"}],
                "model": "claude-haiku-4-5",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 5, "output_tokens": 7}
            })))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "test-key", Duration::from_secs(5));
        let r = b.generate(req("claude-haiku-4-5", "hi")).await.unwrap();
        assert_eq!(r.text, "hello world");
        assert_eq!(r.usage.input_tokens, 5);
        assert_eq!(r.usage.output_tokens, 7);
        assert_eq!(r.usage.provider, "anthropic");
        assert_eq!(r.usage.model, "claude-haiku-4-5");
    }

    #[tokio::test]
    async fn unauthorized_401_maps_to_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"type": "authentication_error", "message": "invalid x-api-key"}
            })))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "bad-key", Duration::from_secs(5));
        let r = b.generate(req("claude-haiku-4-5", "hi")).await;
        let err = r.err().unwrap();
        let typed = err.downcast_ref::<BackendError>().expect("typed BackendError");
        assert!(matches!(typed, BackendError::Unauthorized { provider: "anthropic" }));
    }

    #[tokio::test]
    async fn not_found_404_maps_to_model_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"type": "not_found_error", "message": "model not found"}
            })))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("claude-bogus", "hi")).await;
        let err = r.err().unwrap();
        let typed = err.downcast_ref::<BackendError>().expect("typed BackendError");
        match typed {
            BackendError::ModelNotFound { provider, model } => {
                assert_eq!(*provider, "anthropic");
                assert_eq!(model, "claude-bogus");
            }
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn server_error_500_maps_to_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("claude-haiku-4-5", "hi")).await;
        let err = r.err().unwrap();
        let typed = err.downcast_ref::<BackendError>().expect("typed BackendError");
        assert!(matches!(typed, BackendError::ServerError { status: 500, .. }));
    }

    #[tokio::test]
    async fn rate_limited_429_maps_to_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("claude-haiku-4-5", "hi")).await;
        let err = r.err().unwrap();
        let typed = err.downcast_ref::<BackendError>().expect("typed BackendError");
        assert!(matches!(typed, BackendError::RateLimited { .. }));
    }

    #[tokio::test]
    async fn opus_4_7_drops_temperature() {
        // wiremock matcher on body to confirm temperature is absent.
        use wiremock::matchers::body_json_string;
        let server = MockServer::start().await;
        // We can't easily assert "field absent"; instead match on a body that
        // omits temperature, and verify the request matches.
        Mock::given(method("POST")).and(path("/v1/messages"))
            .and(body_json_string(serde_json::json!({
                "model": "claude-opus-4-7",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hi"}],
                "thinking": {"type": "disabled"}
            }).to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let r = b.generate(req("claude-opus-4-7", "hi")).await.unwrap();
        assert_eq!(r.text, "ok");
        // Mock is strict — if the body included temperature, no mock would match
        // and we'd get a wiremock 404. Reaching here proves temperature was dropped.
    }

    #[tokio::test]
    async fn streaming_bails_in_p1() {
        let b = AnthropicBackend::new("http://127.0.0.1:1", "k", Duration::from_millis(100));
        let r = b.generate_stream(req("claude-haiku-4-5", "hi")).await;
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("P2"));
    }

    /// One real-API integration test gated on ANTHROPIC_API_KEY.
    /// Run via `cargo test -- --ignored` only.
    #[tokio::test]
    #[ignore = "requires ANTHROPIC_API_KEY env var; costs ~$0.0001 per run"]
    async fn live_anthropic_haiku_responds() {
        let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
            panic!("ANTHROPIC_API_KEY must be set to run this --ignored test");
        };
        let b = AnthropicBackend::new("https://api.anthropic.com", &key, Duration::from_secs(30));
        let r = b
            .generate(ChatRequest {
                model: "claude-haiku-4-5",
                system: Some("You answer in exactly one short sentence."),
                user: "What is 2+2?",
                max_tokens: 32,
                temperature: Some(0.0),
                stop: vec![],
                cache_system: false,
                cache_user_prefix: None,
            })
            .await
            .expect("live API call should succeed");
        assert!(!r.text.is_empty());
        assert!(r.usage.input_tokens > 0);
        assert!(r.usage.output_tokens > 0);
        assert_eq!(r.usage.provider, "anthropic");
    }
}
```

Append to `mur-core/src/conversations/backend/mod.rs`:

```rust
pub mod anthropic;
```

**Step 2: Run tests (excluding the `#[ignore]`d live test)**

Run: `cargo test -p mur-core --lib conversations::backend::anthropic 2>&1 | tail -20`

Expected: PASS — 7 tests pass, 1 ignored.

**Step 3: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 4: Commit**

```bash
git add mur-core/src/conversations/backend/anthropic.rs mur-core/src/conversations/backend/mod.rs
git commit -m "$(cat <<'EOF'
feat(backend): add AnthropicBackend (non-streaming) via raw HTTP

Implements ChatBackend over the Anthropic Messages API
(https://api.anthropic.com/v1/messages). No Rust SDK exists, so we
build the request via reqwest with the canonical headers x-api-key /
anthropic-version: 2023-06-01 / content-type: application/json.

- Drops `temperature` for claude-opus-4-7* models (sampling params 400
  on Opus 4.7 per the API).
- Always sends `thinking: {type: \"disabled\"}` to avoid implicit
  adaptive-thinking spend (no-op on older models).
- Maps HTTP errors to typed BackendError variants
  (Unauthorized/ModelNotFound/RateLimited/ServerError/BadResponse).
- generate_stream bails — streaming lands in P2.
- supports_caching() returns false; cache_* hints ignored in P1.

7 wiremock-based unit tests (happy path, all typed-error mappings,
Opus 4.7 temperature-drop verified via strict body matcher) + 1
#[ignore]d live API integration test gated on ANTHROPIC_API_KEY
(costs ~\$0.0001 per run; CI does NOT execute it).

Refs spec §5.2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Wire `factory::build` to compose `Retrying<Anthropic|Ollama>`

**Files:**
- Modify: `mur-core/src/conversations/backend/factory.rs`

**Step 1: Update the failing test for the anthropic arm**

The Task 3 test `anthropic_provider_unwired_in_task_3` currently asserts an error. Update it to expect success:

```rust
    #[test]
    fn anthropic_provider_returns_anthropic_backend_when_key_present() {
        // Use a synthetic env var so the test doesn't need ANTHROPIC_API_KEY.
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_ANTHROPIC_KEY", "synthetic-key") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_ANTHROPIC_KEY".into()),
            timeout_secs: None,
        };
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "anthropic");
        unsafe { std::env::remove_var("MUR_TEST_ANTHROPIC_KEY") };
    }

    #[test]
    fn anthropic_provider_errors_when_key_env_missing() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_TEST_NONEXISTENT_KEY") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_NONEXISTENT_KEY".into()),
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("MUR_TEST_NONEXISTENT_KEY"));
    }

    #[test]
    fn anthropic_provider_errors_when_api_key_env_field_missing() {
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: None,
            timeout_secs: None,
        };
        let r = build(&cfg);
        assert!(r.is_err());
        let err = r.err().unwrap();
        assert!(format!("{err:#}").contains("api_key_env"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn build_wraps_in_retrying_backend_for_real_providers() {
        // Probe via downcast: build() returns Arc<dyn ChatBackend>, which is
        // a RetryingBackend wrapper. The wrapped name should still be the
        // underlying backend's name.
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let cfg = ollama_cfg("http://127.0.0.1:1", 1);
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "ollama");
        // Behaviorally: RetryingBackend forwards provider_name() to inner.
        // We can't introspect type, but we can verify retries don't break the
        // name forwarding contract.
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mock_backend_is_not_wrapped_in_retrying() {
        // Mock should be returned directly so tests get deterministic timing.
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_LLM_MOCK", "1") };
        let cfg = ollama_cfg("http://localhost:11434", 5);
        let b = build(&cfg).unwrap();
        assert_eq!(b.provider_name(), "mock");
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
    }
```

(Remove the old `anthropic_provider_unwired_in_task_3` test.)

**Step 2: Update `factory::build`**

Replace the function body:

```rust
pub fn build(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
    if std::env::var("MUR_LLM_MOCK").is_ok() || std::env::var("MUR_OLLAMA_MOCK").is_ok() {
        tracing::debug!(provider = %cfg.provider, "MUR_LLM_MOCK active — using MockBackend");
        return Ok(Arc::new(MockBackend::new()));
    }
    let inner: Arc<dyn ChatBackend> = match cfg.provider.as_str() {
        "ollama" => {
            let endpoint = cfg.endpoint.as_deref().unwrap_or("http://localhost:11434");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(OllamaBackend::new(endpoint, timeout))
        }
        "anthropic" => {
            let api_key_env = cfg.api_key_env.as_deref().ok_or_else(|| {
                anyhow::anyhow!("anthropic backend requires api_key_env in BackendConfig")
            })?;
            let api_key = std::env::var(api_key_env).map_err(|_| {
                anyhow::anyhow!(
                    "anthropic backend env var {api_key_env} is not set or not readable"
                )
            })?;
            let endpoint = cfg.endpoint.as_deref().unwrap_or("https://api.anthropic.com");
            let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
            Arc::new(super::anthropic::AnthropicBackend::new(endpoint, &api_key, timeout))
        }
        other => bail!("unsupported provider: {other}"),
    };
    Ok(Arc::new(super::retry::RetryingBackend::with_default_policy(inner)))
}
```

**Step 3: Run tests**

```bash
cargo test -p mur-core --lib conversations::backend::factory -- --test-threads=1 2>&1 | tail -20
```

Expected: PASS for all tests (existing 4 + 4 new = 8 in factory module).

**Step 4: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 5: Commit**

```bash
git add mur-core/src/conversations/backend/factory.rs
git commit -m "$(cat <<'EOF'
feat(backend): factory::build composes Retrying<Anthropic|Ollama>

Anthropic provider arm wires up: resolves api_key_env to the actual
key, errors clearly if api_key_env is None or the env var is unset.
Real providers (anthropic, ollama) are wrapped in
RetryingBackend::with_default_policy. MockBackend stays unwrapped so
tests get deterministic timing.

Tests cover: anthropic happy path with synthetic env key, missing-env
error, missing-api_key_env-field error, mock unwrapped, ollama
provider_name forwarding through retry wrapper.

Refs spec §5.4 + §8.1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Wire `compact.extractive` as canary cloud call site

**Files:**
- Modify: `mur-core/src/conversations/summarize/extractive.rs` (call site)
- Modify: `mur-core/src/conversations/summarize/mod.rs` (call to `extract_chunk` — pass backend instead of OllamaClient)

**Step 1: Read the existing call site**

```bash
sed -n '1,80p' /Users/david/Projects/mur/mur-core/src/conversations/summarize/extractive.rs
sed -n '85,140p' /Users/david/Projects/mur/mur-core/src/conversations/summarize/mod.rs
```

You'll see `extract_chunk(client: &OllamaClient, model: &str, chunk: &str, max_spans: u32)` calling `client.generate(...)`. The mod.rs caller constructs an `OllamaClient` with `cfg.ollama_endpoint`.

**Step 2: Refactor `extract_chunk` to take `&dyn ChatBackend`**

In `extractive.rs`:

- Replace `use crate::conversations::ollama::{GenerateOptions, GenerateRequest, OllamaClient};` with `use crate::conversations::backend::{ChatBackend, ChatRequest};`
- Change signature: `pub async fn extract_chunk(backend: &dyn ChatBackend, model: &str, chunk: &str, max_spans: u32) -> Result<...>`
- Change call body from `client.generate(GenerateRequest { ... })` to:

```rust
    let resp = backend
        .generate(ChatRequest {
            model,
            user: &prompt,
            system: None,
            max_tokens: 2048,
            temperature: Some(0.0),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        })
        .await
        .with_context(|| format!("extractive call failed for model {model}"))?;
```

- Change `r.response` references to `r.text`

(Read the actual file to copy the prompt template + JSON parser intact — only the I/O path changes.)

**Step 3: Update `compact_day` in `summarize/mod.rs`**

Replace the `OllamaClient` construction with `factory::build`:

```rust
    let extractive_backend = crate::conversations::backend::factory::build(
        &cfg.synthesize_extractive_backend(),
    )?;
    // ... loop calls: extractive::extract_chunk(extractive_backend.as_ref(), &cfg.extractive_model, ...)
```

If `extract_chunk`'s `model` param now should come from the backend config rather than `cfg.extractive_model`, update accordingly. Check `synthesize_extractive_backend()` — it returns a `BackendConfig` whose `.model` field is the right source.

Update the `extract_chunk` call to pass `&extractive_backend_cfg.model` instead of `&cfg.extractive_model`:

```rust
    let extractive_cfg = cfg.synthesize_extractive_backend();
    let extractive_backend = crate::conversations::backend::factory::build(&extractive_cfg)?;
    // inside the chunk loop:
    let r = extractive::extract_chunk(extractive_backend.as_ref(), &extractive_cfg.model, chunk, cfg.max_extractive_spans).await?;
```

**Step 4: Update existing extractive tests if any**

```bash
grep -n "extract_chunk\|OllamaClient" /Users/david/Projects/mur/mur-core/src/conversations/summarize/extractive.rs /Users/david/Projects/mur/mur-core/src/conversations/summarize/mod.rs | head -20
```

Update test functions in `extractive.rs` to construct via `OllamaBackend::new(...)` like P0's rewriter tests did. Pattern:

```rust
use crate::conversations::backend::ollama::OllamaBackend;
let backend = OllamaBackend::new("http://127.0.0.1:1", Duration::from_millis(200));
extract_chunk(&backend, "qwen3:14b", chunk_text, 5).await
```

**Step 5: Run extractive + summarize tests**

```bash
cargo test -p mur-core --lib conversations::summarize -- --test-threads=1 2>&1 | tail -15
```

Expected: PASS. If integration tests existed using the mock env var (`MUR_OLLAMA_MOCK=1`), they should still pass because `factory::build` honors both `MUR_LLM_MOCK` and `MUR_OLLAMA_MOCK`.

**Step 6: Run integration tests for compact**

```bash
cargo test -p mur-core --test cli_conversations -- --test-threads=1 2>&1 | tail -10
```

Expected: all 24+ pass (no regression in compact pipeline).

**Step 7: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy --workspace --all-targets -- -D warnings
```

**Step 8: Commit**

```bash
git add mur-core/src/conversations/summarize/extractive.rs mur-core/src/conversations/summarize/mod.rs
git commit -m "$(cat <<'EOF'
refactor(compact): wire extractive stage to ChatBackend (canary cloud)

extract_chunk now takes &dyn ChatBackend instead of &OllamaClient.
compact_day constructs the backend via factory::build using
cfg.synthesize_extractive_backend() — which honors per-stage
extractive_backend override and falls back to legacy
extractive_model + ollama_endpoint when None.

This is the second canary call site after P0's ask::rewriter.
Behavior unchanged for users with no per-stage override (still hits
local Ollama). Users who set extractive_backend.provider = anthropic
in config.yaml now get cloud-side extractive summarization.

Refs spec §3 (call-site refactor list) and plan task 7.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Doctor enhancements — cloud provider probes

**Files:**
- Modify: `mur-core/src/cmd/conversations_cmd.rs` (the `cmd_conversations_doctor` function around line 434)

**Step 1: Read the existing doctor**

```bash
sed -n '434,540p' /Users/david/Projects/mur/mur-core/src/cmd/conversations_cmd.rs
```

Note where it does the Ollama `/api/tags` reachability probe (~line 700+). The new logic slots in alongside.

**Step 2: Add a helper that collects unique backend configs**

Add at top of the file (or inside the doctor function):

```rust
fn collect_backend_configs(cfg: &mur_common::Config) -> Vec<mur_common::config::BackendConfig> {
    let mut backends = vec![
        cfg.conversations.compact.synthesize_extractive_backend(),
        cfg.conversations.compact.synthesize_abstractive_backend(),
        cfg.conversations.ask.synthesize_backend(),
        cfg.conversations.ask.synthesize_rewriter_backend(),
    ];
    backends.sort_by(|a, b| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)));
    backends.dedup_by(|a, b| a.provider == b.provider && a.model == b.model);
    backends
}
```

**Step 3: Add the probe block to `cmd_conversations_doctor`**

After the existing Ollama tags probe (search for the line printing `"  ✓ Ollama reachable at"`), add:

```rust
    // ── P1: Cloud provider probes ─────────────────────────────────────────
    let backends = collect_backend_configs(&cfg);
    let cloud_backends: Vec<_> = backends.iter().filter(|b| b.provider == "anthropic").collect();
    if cloud_backends.is_empty() {
        println!("  · no cloud providers in active config (skipping cloud probes)");
    } else {
        for b in cloud_backends {
            // Env-var check
            let key_env = match b.api_key_env.as_deref() {
                Some(e) => e,
                None => {
                    println!("  ✗ anthropic backend for {} has no api_key_env in config", b.model);
                    ok = false;
                    continue;
                }
            };
            match std::env::var(key_env) {
                Ok(v) if !v.is_empty() => {
                    println!("  ✓ anthropic api_key_env {key_env} is set");
                }
                _ => {
                    println!("  ✗ anthropic api_key_env {key_env} is unset or empty");
                    ok = false;
                    continue;
                }
            }
            // Reachability probe (2s timeout, non-fatal)
            let key = std::env::var(key_env).unwrap_or_default();
            let endpoint = b.endpoint.as_deref().unwrap_or("https://api.anthropic.com");
            let url = format!("{}/v1/models/{}", endpoint.trim_end_matches('/'), b.model);
            let client = match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    println!("  ✗ failed to build reqwest client for {endpoint}: {e}");
                    ok = false;
                    continue;
                }
            };
            let resp = client
                .get(&url)
                .header("x-api-key", &key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    println!("  ✓ anthropic model {} reachable at {endpoint}", b.model);
                }
                Ok(r) => {
                    println!(
                        "  ✗ anthropic model {} returned {} at {endpoint}",
                        b.model,
                        r.status()
                    );
                    ok = false;
                }
                Err(e) => {
                    println!("  ✗ anthropic probe for {} failed: {e}", b.model);
                    ok = false;
                }
            }
        }
    }
```

(`ok` is the existing `let mut ok = true;` flag in the doctor function — its final value drives the exit code.)

**Step 4: Manual smoke test — no automated test for doctor probes**

Doctor doesn't have unit tests today (it's a `println!`-driven UX command). Manual verification only:

```bash
# Should print "no cloud providers" (default config has no anthropic)
cargo run --bin mur -- conversations doctor 2>&1 | tail -20

# With a cloud override in ~/.mur/config.yaml + key set, should print probe results
ANTHROPIC_API_KEY=sk-ant-... cargo run --bin mur -- conversations doctor 2>&1 | tail -20
```

If the doctor prints sensible probe output and exits 0 / 1 appropriately, accept.

**Step 5: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy --workspace -- -D warnings
```

**Step 6: Commit**

```bash
git add mur-core/src/cmd/conversations_cmd.rs
git commit -m "$(cat <<'EOF'
feat(doctor): add cloud-provider reachability + API-key probes

cmd_conversations_doctor now collects unique BackendConfig entries
across all four conversations call sites (compact.{extractive,
abstractive}, ask.{backend, rewriter_backend}), filters to anthropic
providers, and for each:
  1. Verifies api_key_env field is set in config and the named env
     var is non-empty
  2. Probes GET /v1/models/{model} with 2s timeout to confirm
     reachability + key validity + model existence

Each probe is non-fatal — failures mark the doctor exit code as
non-zero but other checks continue. Skipped silently if no anthropic
provider is in the active config (default Ollama-only setup).

Refs spec §8.3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: End-to-end verification (no commit)

**Files:** none modified.

**Step 1: Full workspace build**

```bash
cargo build --workspace
```

Expected: clean.

**Step 2: Full workspace test (excluding `#[ignore]`d live test)**

```bash
cargo test --workspace -- --test-threads=1
```

Expected: all tests pass.

**Step 3: Workspace clippy + fmt**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: both clean.

**Step 4: Smoke test compact with mock**

```bash
MUR_LLM_MOCK=1 cargo run --bin mur -- conversations compact --max-days 1 2>&1 | tail -20
```

Expected: command exits 0, prints either "(nothing to compact)" if no transcripts, or per-day summaries.

**Step 5: Smoke test compact end-to-end with synthetic anthropic config**

Create a temp config:
```bash
TMPDIR=$(mktemp -d)
cat > $TMPDIR/config.yaml <<'YAML'
# Minimal config for verifying anthropic wiring
embedding:
  provider: ollama
  model: qwen3-embedding:0.6b
  dimensions: 1024
  ollama_endpoint: http://localhost:11434
conversations:
  compact:
    extractive_backend:
      provider: anthropic
      model: claude-haiku-4-5
      api_key_env: ANTHROPIC_API_KEY
YAML
HOME=$TMPDIR ANTHROPIC_API_KEY=stub-not-real cargo run --bin mur -- conversations doctor 2>&1 | tail -20
```

Expected: doctor reports `✗ anthropic api_key_env ANTHROPIC_API_KEY is set` (true), then `✗ anthropic probe for claude-haiku-4-5 failed: ...` (because `stub-not-real` isn't a valid key — that's the expected failure mode showing the probe is wired).

**Step 6: Smoke test live anthropic** (only if `ANTHROPIC_API_KEY` is set with a real key)

```bash
cargo test -p mur-core --lib --tests conversations::backend::anthropic::tests::live_anthropic_haiku_responds -- --ignored --nocapture
```

Expected: passes — confirms the AnthropicBackend talks to the real API correctly.

**Step 7: Report**

Summary for human reviewer:
- 9 commits on `feat/cloud-llm-backend-p1-plan` after the docs commit
- New: `BackendConfig` in mur-common, per-stage overrides on CompactConfig+AskConfig, RetryingBackend decorator, AnthropicBackend (~250 LOC), doctor cloud probes
- Replaced: P0 BackendSpec → mur-common BackendConfig
- Behavior: identical for users with legacy config (no per-stage `*_backend` field). Users who add `extractive_backend.provider = anthropic` get cloud extractive on next compact.
- Test count delta: ~+30 tests. Live API test gated `#[ignore]`.

---

## Out of scope — explicitly deferred to P2+

Do **not** implement any of these in P1:

- `AnthropicBackend::generate_stream` (SSE parser) — P2
- Migrating `compact.abstractive`, `summarize::rollup`, `ask::generate`, `ask::abstractive::compress_hit` — P2/P3
- `RetryingBackend` retry on `generate_stream` (mid-stream is its own design) — P2
- Prompt caching wiring (`cache_system` / `cache_user_prefix` hint plumbing on AnthropicBackend) — P3
- Cost telemetry, `mur conversations cost-report` — P3
- Migrating `learn`/`extract_llm` onto `ChatBackend`, deleting `mur-core/src/llm.rs` — P4
- Keychain integration (`secret_ref` field on BackendConfig) — P3
- Switching default to `claude-opus-4-7` — keep defaults at `claude-haiku-4-5`/`claude-sonnet-4-6` per spec §7
- Doctor unit tests — doctor stays manually verified until a broader CLI testing harness lands

If an instruction in this plan tempts you to touch one of these, **stop and ask** — it means the plan or spec needs amendment, not extra work.

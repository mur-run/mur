# Hub Unified Model Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One-page model management in Hub Settings + first-run model wizard + `mur init` model setup collapsed to one question, with keychain keys working everywhere via `api_key_ref`.

**Architecture:** Registry (`~/.mur/models.yaml`) stays the catalog; `~/.mur/config.yaml` slots store resolved provider/model plus a `SecretRef` string (`api_key_ref`). A shared pure engine `mur-core::model_setup::recommend/apply` powers both `mur init` and the Hub wizard. Hub tauri commands call mur-core directly (the Hub crate already depends on `mur-core`).

**Tech Stack:** Rust (mur-common, mur-core), Tauri 2 commands, React + TS (mur-hub-gui/ui), vitest.

**Spec:** `docs/superpowers/specs/2026-07-04-hub-unified-model-setup-design.md`

## Global Constraints

- Rust builds/tests need: `export ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist` (mur-core won't compile/link without them).
- Run targeted tests only: `cargo test -p <crate> <filter>` or `cargo nextest run -p <crate> <filter>`. NEVER bare `cargo test --workspace` (7 known-flaky mur-core tests).
- Lint gate per task: `cargo clippy -p <touched crates> -- -D warnings` and `cargo fmt --all` before every commit (CI rustfmt is stricter than local — always run fmt).
- Hub Rust needs `mur-hub-gui/ui/dist/index.html` to exist for clippy/check (stub `<!doctype html>` if missing; do NOT commit the stub).
- Single source file ≤ 800 lines (CLAUDE.md rule 4). `init.rs` is 1581 — this plan only shrinks it.
- User-facing brand string is uppercase **MUR** (CLAUDE.md rule 7). All new Hub UI strings go in BOTH `ui/src/i18n/en.ts` and `ui/src/i18n/zh-TW.ts`.
- No hardcoded magic values without a named const.
- Branch stack: PR1 on `feat/model-setup-keyref` (base: `feat/hub-unified-model-setup`), PR2 `feat/hub-model-slots` (base: PR1), PR3 `feat/hub-model-wizard` (base: PR2). Merge with merge-commit, not squash (stacked PRs; squash breaks children).
- Commit messages: conventional (`feat:`, `docs:`), end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

# PR 1 — `api_key_ref` + recommend engine + one-question init

### Task 1: `api_key_ref` field on the three config structs

**Files:**
- Modify: `mur-common/src/config.rs` (LlmConfig ~line 296, EmbeddingConfig ~line 258, BackendConfig ~line 365, `to_backend_config` ~line 337, both Default impls)
- Test: same file, `#[cfg(test)] mod` at bottom

**Interfaces:**
- Produces: `LlmConfig.api_key_ref: Option<String>`, `EmbeddingConfig.api_key_ref: Option<String>`, `BackendConfig.api_key_ref: Option<String>` — all serde-default `None`, skipped when None. `LlmConfig::to_backend_config()` carries it 1:1.

- [ ] **Step 1: Write the failing test** (append to the existing test mod in `config.rs`; find it with `rg -n "mod tests" mur-common/src/config.rs`)

```rust
#[test]
fn api_key_ref_roundtrips_and_defaults_none() {
    // Old YAML without the field still parses, field defaults to None.
    let b: BackendConfig =
        serde_yaml_ng::from_str("provider: anthropic\nmodel: m\n").unwrap();
    assert_eq!(b.api_key_ref, None);
    let l: LlmConfig = serde_yaml_ng::from_str("provider: anthropic\nmodel: m\n").unwrap();
    assert_eq!(l.api_key_ref, None);
    let e: EmbeddingConfig = serde_yaml_ng::from_str("provider: ollama\nmodel: m\n").unwrap();
    assert_eq!(e.api_key_ref, None);

    // Set → survives YAML round-trip and to_backend_config.
    let mut l2 = LlmConfig::default();
    l2.api_key_ref = Some("keychain:mur/anthropic".into());
    let y = serde_yaml_ng::to_string(&l2).unwrap();
    let l3: LlmConfig = serde_yaml_ng::from_str(&y).unwrap();
    assert_eq!(l3.api_key_ref.as_deref(), Some("keychain:mur/anthropic"));
    assert_eq!(
        l3.to_backend_config().api_key_ref.as_deref(),
        Some("keychain:mur/anthropic")
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common api_key_ref_roundtrips`
Expected: FAIL — `no field api_key_ref`

- [ ] **Step 3: Add the field to all three structs + carry it**

In each of `LlmConfig`, `EmbeddingConfig` (right after their `api_key_env` field):

```rust
    /// SecretRef string for the API key (e.g. "keychain:mur/anthropic",
    /// "env:ANTHROPIC_API_KEY"). Takes precedence over `api_key_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,
```

In `BackendConfig` (struct is `#[serde(default)]`, so no per-field attr needed):

```rust
    /// SecretRef string for the API key. Takes precedence over `api_key_env`.
    pub api_key_ref: Option<String>,
```

Add `api_key_ref: None,` to `EmbeddingConfig::default()`, `LlmConfig`'s Default (if explicit), and `BackendConfig::default()`. In `to_backend_config()` add `api_key_ref: self.api_key_ref.clone(),`. Fix any struct-literal construction sites the compiler flags (`cargo check -p mur-common -p mur-core 2>&1 | head -50`) by adding `api_key_ref: None` — BackendConfig literals exist in `mur-core/src/conversations/` tests and `skill_llm/mod.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-common api_key_ref_roundtrips && cargo check -p mur-core`
Expected: PASS / clean check

- [ ] **Step 5: Commit**

```bash
git checkout -b feat/model-setup-keyref
cargo fmt --all && git add -A mur-common mur-core && git commit -m "feat(config): api_key_ref SecretRef field on LLM/embedding/backend configs"
```

### Task 2: `SecretRef::resolve_blocking`

**Files:**
- Modify: `mur-common/src/secret.rs` (impl SecretRef, after `resolve_to_string` ~line 146)

**Interfaces:**
- Consumes: existing `async fn resolve()`.
- Produces: `pub fn resolve_blocking(&self) -> Result<SecretString, SecretError>` and `pub fn resolve_to_string_blocking(&self) -> Option<String>`.

- [ ] **Step 1: Write the failing test** (append to the test mod in `secret.rs`)

```rust
#[test]
fn resolve_blocking_env_and_missing() {
    unsafe { std::env::set_var("MUR_TEST_SECRET_BLOCKING", "s3cret") };
    let r: SecretRef = "env:MUR_TEST_SECRET_BLOCKING".parse().unwrap();
    assert_eq!(r.resolve_to_string_blocking().as_deref(), Some("s3cret"));
    unsafe { std::env::remove_var("MUR_TEST_SECRET_BLOCKING") };
    assert!(r.resolve_blocking().is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common resolve_blocking_env`
Expected: FAIL — method not found

- [ ] **Step 3: Implement** (mirrors `block_on_in_runtime` in `mur-core/src/cmd/init.rs:12` — same panic-safety reasoning)

```rust
    /// Synchronous resolve for callers outside an async context (CLI
    /// factories, config loaders). Inside a multi-thread tokio runtime it
    /// uses block_in_place; otherwise it spins a current-thread runtime.
    pub fn resolve_blocking(&self) -> Result<SecretString, SecretError> {
        match tokio::runtime::Handle::try_current() {
            Ok(h) => tokio::task::block_in_place(|| h.block_on(self.resolve())),
            Err(_) => tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| SecretError::KeychainBackend(format!("runtime: {e}")))?
                .block_on(self.resolve()),
        }
    }

    /// Blocking analogue of `resolve_to_string` — same materialization
    /// caveats apply.
    pub fn resolve_to_string_blocking(&self) -> Option<String> {
        use secrecy::ExposeSecret;
        self.resolve_blocking()
            .ok()
            .map(|s| s.expose_secret().to_string())
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-common resolve_blocking_env`
Expected: PASS

- [ ] **Step 5: Commit** — `git add mur-common && git commit -m "feat(secret): SecretRef::resolve_blocking for sync callers"`

### Task 3: Factory + skill_llm honor `api_key_ref`

**Files:**
- Modify: `mur-core/src/conversations/backend/factory.rs:63-74` (`resolve_api_key`)
- Modify: `mur-core/src/skill_llm/mod.rs:102-114` (`backend_config_from_entry`)

**Interfaces:**
- Consumes: `BackendConfig.api_key_ref` (Task 1), `SecretRef::resolve_to_string_blocking` (Task 2).
- Produces: `resolve_api_key` tries ref first; `backend_config_from_entry` maps `entry.secret` → `api_key_ref` (fixes the current silent drop of keychain refs).

- [ ] **Step 1: Write the failing test** (append to the test mod in `factory.rs`; follow its `ENV_LOCK` + `unsafe env` pattern exactly)

```rust
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn api_key_ref_takes_precedence_over_env() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_REF_KEY", "key-from-ref") };
        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("MUR_TEST_NONEXISTENT_KEY".into()),
            api_key_ref: Some("env:MUR_TEST_REF_KEY".into()),
            timeout_secs: None,
        };
        // ref resolves → build succeeds even though api_key_env is unset
        assert!(build(&cfg).is_ok());
        unsafe { std::env::remove_var("MUR_TEST_REF_KEY") };
        // ref no longer resolves → error mentions the ref
        let err = format!("{:#}", build(&cfg).err().unwrap());
        assert!(err.contains("MUR_TEST_REF_KEY"), "err was: {err}");
    }
```

Note: `flavor = "multi_thread"` is required — `block_in_place` panics on a current-thread runtime. Match the file's existing test attribute style if it already sets a flavor.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core api_key_ref_takes_precedence`
Expected: FAIL (missing field first; after adding `api_key_ref: None` to other test literals in Task 1 it fails on the assert)

- [ ] **Step 3: Implement `resolve_api_key`**

```rust
fn resolve_api_key(cfg: &BackendConfig) -> Result<String> {
    if let Some(r) = cfg.api_key_ref.as_deref() {
        let sref: mur_common::secret::SecretRef = r
            .parse()
            .map_err(|e| anyhow::anyhow!("{} backend api_key_ref invalid: {e}", cfg.provider))?;
        return sref.resolve_to_string_blocking().ok_or_else(|| {
            anyhow::anyhow!(
                "{} backend api_key_ref {r} did not resolve (and no usable api_key_env fallback was attempted — fix or remove api_key_ref)",
                cfg.provider
            )
        });
    }
    let env_var = cfg
        .api_key_env
        .as_deref()
        .unwrap_or_else(|| default_key_env(&cfg.provider));
    std::env::var(env_var).map_err(|_| {
        anyhow::anyhow!(
            "{} backend env var {env_var} is not set or not readable",
            cfg.provider
        )
    })
}
```

(Explicit ref that fails = hard error, not silent env fallback — a configured keychain ref that stopped resolving should be surfaced, not masked.)

In `skill_llm/mod.rs` `backend_config_from_entry`, replace the Env-only mapping:

```rust
fn backend_config_from_entry(entry: &ModelEntry) -> BackendConfig {
    let api_key_env = entry.secret.as_ref().and_then(|s| match s {
        SecretRef::Env(var) => Some(var.clone()),
        _ => None,
    });
    BackendConfig {
        provider: entry.provider.clone(),
        model: entry.model.clone(),
        endpoint: entry.base_url.clone(),
        api_key_env,
        // Keychain/file/cmd refs previously dropped silently — now carried.
        api_key_ref: entry.secret.as_ref().map(|s| s.to_string()),
        timeout_secs: None,
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-core api_key_ref_takes_precedence && cargo test -p mur-core --lib skill_llm && cargo test -p mur-core backend`
Expected: PASS (existing env-path factory tests unchanged = regression gate)

- [ ] **Step 5: Commit** — `git add mur-core && git commit -m "feat(llm): resolve api_key_ref before api_key_env in backend factory + skill_llm"`

### Task 4: Embedding + `has_llm_config` honor `api_key_ref`

**Files:**
- Modify: `mur-core/src/store/embedding.rs:26-45` (`EmbeddingConfig::from_config`)
- Modify: `mur-core/src/extract_llm.rs:264-285` (`has_llm_config`)

**Interfaces:**
- Consumes: `config.embedding.api_key_ref`, `config.llm.api_key_ref`, `resolve_to_string_blocking`.

- [ ] **Step 1: Write the failing test** (append to test mod in `store/embedding.rs`; create one if absent)

```rust
#[test]
fn from_config_prefers_api_key_ref() {
    unsafe { std::env::set_var("MUR_TEST_EMB_REF", "emb-key") };
    let mut cfg = mur_common::config::Config::default();
    cfg.embedding.provider = "openai".into();
    cfg.embedding.api_key_ref = Some("env:MUR_TEST_EMB_REF".into());
    let ec = EmbeddingConfig::from_config(&cfg);
    match ec.provider {
        EmbeddingProvider::OpenAI { api_key, .. } => assert_eq!(api_key, "emb-key"),
        _ => panic!("expected OpenAI provider"),
    }
    unsafe { std::env::remove_var("MUR_TEST_EMB_REF") };
}
```

- [ ] **Step 2: Run to verify it fails**: `cargo test -p mur-core from_config_prefers_api_key_ref` — FAIL (ref ignored, falls to OPENAI_API_KEY/empty)

- [ ] **Step 3: Implement.** In `from_config`, replace the api_key chain:

```rust
                let api_key = cfg
                    .embedding
                    .api_key_ref
                    .as_deref()
                    .and_then(|r| r.parse::<mur_common::secret::SecretRef>().ok())
                    .and_then(|s| s.resolve_to_string_blocking())
                    .or_else(|| {
                        cfg.embedding
                            .api_key_env
                            .as_deref()
                            .and_then(|env| std::env::var(env).ok())
                    })
                    .unwrap_or_else(|| std::env::var("OPENAI_API_KEY").unwrap_or_default());
```

In `has_llm_config`, before the env-var check add:

```rust
    if let Some(r) = llm.api_key_ref.as_deref() {
        return r
            .parse::<mur_common::secret::SecretRef>()
            .map(|s| s.resolve_to_string_blocking().is_some())
            .unwrap_or(false);
    }
```

- [ ] **Step 4: Run**: `cargo test -p mur-core from_config_prefers_api_key_ref && cargo test -p mur-core has_llm` — PASS

- [ ] **Step 5: Commit** — `git add mur-core && git commit -m "feat(llm): embedding client and has_llm_config honor api_key_ref"`

### Task 5: `model_setup` module — `recommend` / `apply` / predicates

**Files:**
- Create: `mur-core/src/model_setup/mod.rs`
- Modify: `mur-core/src/lib.rs` AND `mur-core/src/main.rs` (add `pub mod model_setup;` — gotcha: mur-core modules must be declared in BOTH)

**Interfaces:**
- Consumes: `crate::discovery::{DiscoveredModel, Backend, ModelKind}`, `crate::discovery::aggregate::{build_llm_menu, build_embedding_menu, MenuRowKind}`, `mur_common::model::ModelRegistry`, `mur_common::config::Config`.
- Produces (used by Tasks 6, 8, 11):
  - `pub struct KeySource { pub provider: String, pub api_key_ref: String, pub base_url: Option<String> }`
  - `pub struct SlotChoice { pub provider: String, pub model: String, pub openai_url: Option<String>, pub api_key_ref: Option<String> }`
  - `pub struct SearchChoice { pub provider: String, pub model: String, pub dimensions: usize, pub openai_url: Option<String>, pub api_key_ref: Option<String> }`
  - `pub struct ModelSetupPlan { pub smart: Option<SlotChoice>, pub search: Option<SearchChoice>, pub conversations_model: Option<String>, pub summary: String }` (all Serialize/Deserialize)
  - `pub fn probe_env_keys() -> Vec<KeySource>`
  - `pub fn keychain_key_sources(reg: &ModelRegistry) -> Vec<KeySource>`
  - `pub fn recommend(discovered: &[DiscoveredModel], keys: &[KeySource]) -> ModelSetupPlan`
  - `pub fn apply(plan: &ModelSetupPlan, config: &mut mur_common::config::Config)`
  - `pub fn is_factory_default_models(config: &mur_common::config::Config) -> bool`
  - `pub fn fallback_dims_for(id: &str) -> Option<usize>` (MOVED here from `cmd/init.rs:27`; init re-exports or calls this)

- [ ] **Step 1: Write the failing tests** (in `model_setup/mod.rs` test mod — write module skeleton + tests first)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{Backend, DiscoveredModel, ModelKind};

    fn local_llm(id: &str) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            backend: Backend::Ollama,
            kind: ModelKind::Llm,
            dims: None,
            family: None,
            size_bytes: None,
            probed_at: None,
        }
    }
    fn local_emb(id: &str, dims: usize) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            backend: Backend::Ollama,
            kind: ModelKind::Embedding,
            dims: Some(dims),
            family: None,
            size_bytes: None,
            probed_at: None,
        }
    }
    fn anthropic_key() -> KeySource {
        KeySource {
            provider: "anthropic".into(),
            api_key_ref: "keychain:mur/anthropic".into(),
            base_url: None,
        }
    }

    #[test]
    fn cloud_key_plus_local_runtime_is_hybrid() {
        let d = vec![local_llm("qwen3.5:4b"), local_emb("qwen3-embedding:0.6b", 1024)];
        let plan = recommend(&d, &[anthropic_key()]);
        let smart = plan.smart.unwrap();
        assert_eq!(smart.provider, "anthropic");
        assert_eq!(smart.model, "claude-opus-4-6");
        assert_eq!(smart.api_key_ref.as_deref(), Some("keychain:mur/anthropic"));
        let search = plan.search.unwrap();
        assert_eq!(search.provider, "ollama");
        assert_eq!(search.dimensions, 1024);
        assert_eq!(plan.conversations_model.as_deref(), Some("qwen3.5:4b"));
    }

    #[test]
    fn no_key_falls_back_to_local_llm() {
        let d = vec![local_llm("qwen3.5:4b")];
        let plan = recommend(&d, &[]);
        let smart = plan.smart.unwrap();
        assert_eq!(smart.provider, "ollama");
        assert_eq!(smart.model, "qwen3.5:4b");
        assert_eq!(smart.api_key_ref, None);
    }

    #[test]
    fn nothing_detected_yields_empty_plan_with_honest_summary() {
        let plan = recommend(&[], &[]);
        assert!(plan.smart.is_none());
        assert!(plan.search.is_none());
        assert!(plan.summary.contains("MUR Hub"));
    }

    #[test]
    fn openrouter_key_maps_to_openai_compat() {
        let plan = recommend(
            &[],
            &[KeySource {
                provider: "openrouter".into(),
                api_key_ref: "env:OPENROUTER_API_KEY".into(),
                base_url: None,
            }],
        );
        let smart = plan.smart.unwrap();
        assert_eq!(smart.provider, "openai");
        assert_eq!(smart.model, "google/gemini-2.5-flash");
        assert_eq!(smart.openai_url.as_deref(), Some("https://openrouter.ai/api/v1"));
    }

    #[test]
    fn apply_writes_all_slots() {
        let d = vec![local_llm("qwen3.5:4b"), local_emb("qwen3-embedding:0.6b", 1024)];
        let plan = recommend(&d, &[anthropic_key()]);
        let mut cfg = mur_common::config::Config::default();
        apply(&plan, &mut cfg);
        assert_eq!(cfg.llm.provider, "anthropic");
        assert_eq!(cfg.llm.api_key_ref.as_deref(), Some("keychain:mur/anthropic"));
        assert_eq!(cfg.embedding.model, "qwen3-embedding:0.6b");
        assert_eq!(cfg.embedding.dimensions, 1024);
        assert_eq!(cfg.conversations.ask.model, "qwen3.5:4b");
        assert_eq!(cfg.conversations.compact.extractive_model, "qwen3.5:4b");
        assert_eq!(cfg.conversations.rollup.extractive_model, "qwen3.5:4b");
        assert!(!is_factory_default_models(&cfg));
    }

    #[test]
    fn factory_default_predicate() {
        assert!(is_factory_default_models(&mur_common::config::Config::default()));
    }
}
```

- [ ] **Step 2: Run to verify they fail**: `cargo test -p mur-core model_setup` — FAIL (module missing)

- [ ] **Step 3: Implement `model_setup/mod.rs`**

```rust
//! Shared model-setup recommendation engine.
//!
//! One deterministic policy used by BOTH `mur init` (one-question Step G)
//! and the Hub first-run wizard: cloud smart model when a key is available,
//! local embedding + conversations when a local runtime exists. Pure
//! functions — probing (env/keychain/discovery) happens in the callers'
//! gather helpers so `recommend` is unit-testable.

use serde::{Deserialize, Serialize};

use crate::discovery::aggregate::{MenuRowKind, build_embedding_menu, build_llm_menu};
use crate::discovery::{Backend, DiscoveredModel};
use mur_common::config::Config;
use mur_common::model::ModelRegistry;

/// A provider we can authenticate against, and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeySource {
    pub provider: String,
    /// SecretRef wire string ("env:VAR" or "keychain:service/account").
    pub api_key_ref: String,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotChoice {
    pub provider: String,
    pub model: String,
    pub openai_url: Option<String>,
    pub api_key_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchChoice {
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
    pub openai_url: Option<String>,
    pub api_key_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSetupPlan {
    /// None = nothing usable detected; leave config untouched.
    pub smart: Option<SlotChoice>,
    pub search: Option<SearchChoice>,
    /// Local model for ask/compact/rollup. None = leave untouched.
    pub conversations_model: Option<String>,
    /// One-line human preview shown by init and the wizard.
    pub summary: String,
}

/// Deterministic priority: first table row whose key is available wins.
/// (cfg_provider, model, openai_url) mirror the values init historically
/// wrote for each provider choice.
struct CloudDefault {
    key_provider: &'static str,
    cfg_provider: &'static str,
    model: &'static str,
    openai_url: Option<&'static str>,
}
const CLOUD_LLM_DEFAULTS: &[CloudDefault] = &[
    CloudDefault { key_provider: "anthropic", cfg_provider: "anthropic", model: "claude-opus-4-6", openai_url: None },
    CloudDefault { key_provider: "openai", cfg_provider: "openai", model: "gpt-4o-mini", openai_url: None },
    CloudDefault { key_provider: "gemini", cfg_provider: "gemini", model: "gemini-2.5-flash", openai_url: None },
    CloudDefault { key_provider: "openrouter", cfg_provider: "openai", model: "google/gemini-2.5-flash", openai_url: Some("https://openrouter.ai/api/v1") },
];

/// (provider, embedding model, dims) — same table init's cloud-embedding
/// prompt used. Keyed by the SMART provider's key_provider.
const CLOUD_EMBEDDING_DEFAULTS: &[(&str, &str, &str, usize)] = &[
    ("openai", "openai", "text-embedding-3-small", 1536),
    ("gemini", "gemini", "text-embedding-004", 768),
    ("anthropic", "anthropic", "voyage-3-lite", 1024),
];

const ENV_KEY_TABLE: &[(&str, &str)] = &[
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("gemini", "GEMINI_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
];

/// Detect API keys present as env vars.
pub fn probe_env_keys() -> Vec<KeySource> {
    ENV_KEY_TABLE
        .iter()
        .filter(|(_, var)| std::env::var(var).is_ok_and(|v| !v.is_empty()))
        .map(|(p, var)| KeySource {
            provider: (*p).to_string(),
            api_key_ref: format!("env:{var}"),
            base_url: None,
        })
        .collect()
}

/// Detect providers connected via the Hub (registry entries carrying a
/// secret that actually resolves). First entry per provider wins (BTreeMap
/// order = deterministic).
pub fn keychain_key_sources(reg: &ModelRegistry) -> Vec<KeySource> {
    let mut seen = std::collections::BTreeSet::new();
    reg.models
        .values()
        .filter_map(|e| {
            let s = e.secret.as_ref()?;
            if !seen.insert(e.provider.clone()) {
                return None;
            }
            s.resolve_blocking().ok()?;
            Some(KeySource {
                provider: e.provider.clone(),
                api_key_ref: s.to_string(),
                base_url: e.base_url.clone(),
            })
        })
        .collect()
}

fn best_local_llm(discovered: &[DiscoveredModel]) -> Option<DiscoveredModel> {
    build_llm_menu(discovered)
        .into_iter()
        .find(|r| r.kind == MenuRowKind::Auto)
        .and_then(|r| r.model)
}

fn best_local_embedding(discovered: &[DiscoveredModel]) -> Option<DiscoveredModel> {
    build_embedding_menu(discovered)
        .into_iter()
        .find(|r| r.kind == MenuRowKind::Auto)
        .and_then(|r| r.model)
}

/// Mirrors init_local's runtime→config mapping for a local LLM.
fn local_slot_choice(m: &DiscoveredModel) -> SlotChoice {
    match m.backend {
        Backend::Ollama => SlotChoice {
            provider: "ollama".into(),
            model: m.id.clone(),
            openai_url: None,
            api_key_ref: None,
        },
        Backend::OMlx => SlotChoice {
            provider: "openai".into(),
            model: m.id.clone(),
            openai_url: Some("http://localhost:8000/v1".into()),
            api_key_ref: Some("env:OMLX_API_KEY".into()),
        },
    }
}

pub fn recommend(discovered: &[DiscoveredModel], keys: &[KeySource]) -> ModelSetupPlan {
    let cloud = CLOUD_LLM_DEFAULTS
        .iter()
        .find_map(|d| keys.iter().find(|k| k.provider == d.key_provider).map(|k| (d, k)));
    let local_llm = best_local_llm(discovered);
    let local_emb = best_local_embedding(discovered);

    let smart = match (&cloud, &local_llm) {
        (Some((d, k)), _) => Some(SlotChoice {
            provider: d.cfg_provider.into(),
            model: d.model.into(),
            openai_url: d.openai_url.map(String::from),
            api_key_ref: Some(k.api_key_ref.clone()),
        }),
        (None, Some(m)) => Some(local_slot_choice(m)),
        (None, None) => None,
    };

    let search = match &local_emb {
        Some(m) => Some(SearchChoice {
            provider: match m.backend {
                Backend::Ollama => "ollama".into(),
                Backend::OMlx => "omlx".into(),
            },
            model: m.id.clone(),
            dimensions: m.dims.or_else(|| fallback_dims_for(&m.id)).unwrap_or(1024),
            openai_url: match m.backend {
                Backend::Ollama => None,
                Backend::OMlx => Some("http://localhost:8000/v1".into()),
            },
            api_key_ref: None,
        }),
        None => cloud.as_ref().and_then(|(d, k)| {
            CLOUD_EMBEDDING_DEFAULTS
                .iter()
                .find(|(kp, ..)| *kp == d.key_provider)
                .map(|(_, provider, model, dims)| SearchChoice {
                    provider: (*provider).into(),
                    model: (*model).into(),
                    dimensions: *dims,
                    openai_url: None,
                    api_key_ref: Some(k.api_key_ref.clone()),
                })
        }),
    };

    let conversations_model = local_llm.as_ref().map(|m| m.id.clone());

    let summary = match (&smart, &search) {
        (None, _) => "no models detected — connect a provider in MUR Hub → Settings → Models".into(),
        (Some(s), Some(e)) => format!("{}/{} (smart) + {}/{} (search)", s.provider, s.model, e.provider, e.model),
        (Some(s), None) => format!("{}/{} (smart); no embedding runtime found — search stays unconfigured", s.provider, s.model),
    };

    ModelSetupPlan { smart, search, conversations_model, summary }
}

/// Write the plan into config. Untouched slots (None) keep their values.
pub fn apply(plan: &ModelSetupPlan, config: &mut Config) {
    if let Some(s) = &plan.smart {
        config.llm.provider = s.provider.clone();
        config.llm.model = s.model.clone();
        config.llm.openai_url = s.openai_url.clone();
        config.llm.api_key_ref = s.api_key_ref.clone();
    }
    if let Some(e) = &plan.search {
        config.embedding.provider = e.provider.clone();
        config.embedding.model = e.model.clone();
        config.embedding.dimensions = e.dimensions;
        config.embedding.openai_url = e.openai_url.clone();
        config.embedding.api_key_ref = e.api_key_ref.clone();
    }
    if let Some(m) = &plan.conversations_model {
        config.conversations.ask.model = m.clone();
        config.conversations.compact.extractive_model = m.clone();
        config.conversations.rollup.extractive_model = m.clone();
    }
}

/// True when the model slots still carry factory defaults (wizard trigger).
pub fn is_factory_default_models(config: &Config) -> bool {
    let d = Config::default();
    config.llm.provider == d.llm.provider
        && config.llm.model == d.llm.model
        && config.embedding.provider == d.embedding.provider
        && config.embedding.model == d.embedding.model
}

/// Best-effort hardcoded dims for known embedding model ids (moved from
/// cmd/init.rs so the engine and init share one table).
pub fn fallback_dims_for(id: &str) -> Option<usize> {
    // MOVE the existing body of cmd/init.rs::fallback_dims_for here verbatim.
    if id.contains("Qwen3-Embedding-0.6B") || id.contains("qwen3-embedding:0.6b") {
        return Some(1024);
    }
    // …(rest of the existing arms, copied verbatim — read cmd/init.rs:27-45)…
    None
}
```

Adjust the `LlmConfig` field name for the OpenAI-compat URL if it differs (`rg -n "openai_url" mur-common/src/config.rs` — LlmConfig uses `openai_url`). Declare `pub mod model_setup;` in BOTH `lib.rs` and `main.rs`.

- [ ] **Step 4: Run**: `cargo test -p mur-core model_setup` — all 6 PASS. Fix `build_llm_menu` input expectations if the Auto row needs pre-filtering (tests will tell).

- [ ] **Step 5: Commit** — `git add mur-core && git commit -m "feat(model-setup): shared recommend/apply engine for init and Hub wizard"`

### Task 6: `mur init` Step G → one question

**Files:**
- Modify: `mur-core/src/cmd/init.rs` — replace lines ~895–1166 (from the `// ─── Step G:` comment through the closing `}` of the 4-way `match`, just before `// ─── Step H: Community sharing`); delete `select_conversations_models` (line 51), `select_local_embedding` (line 125), `apply_conversations_model` (line 45), `fallback_dims_for` (line 27, moved in Task 5), and the `select_local_embedding_tests` mod (line 1418).

**Interfaces:**
- Consumes: `crate::model_setup::{probe_env_keys, keychain_key_sources, recommend, apply}`, existing `discover_blocking`, `crate::store::config::{load_config, save_config}`, `mur_common::model::ModelRegistry`.

- [ ] **Step 1: Replace Step G** with:

```rust
    // ─── Step G: Model setup (one question) ─────────────────────────
    println!();
    println!("Model setup:");
    let discovered = discover_blocking(refresh_discovery).unwrap_or_default();
    let mut keys = crate::model_setup::probe_env_keys();
    if let Ok(reg_path) = mur_common::model::ModelRegistry::default_path()
        && let Ok(reg) = mur_common::model::ModelRegistry::load_from(&reg_path)
    {
        keys.extend(crate::model_setup::keychain_key_sources(&reg));
    }
    let plan = crate::model_setup::recommend(&discovered, &keys);
    println!("  1) Use recommended defaults — {}  (default)", plan.summary);
    println!("  2) Configure later in MUR Hub (Settings → Models)");
    print!("Choose [1/2] (default: 1): ");
    io::stdout().flush()?;
    let mut model_choice = String::new();
    io::stdin().read_line(&mut model_choice)?;
    if model_choice.trim() == "2" || plan.smart.is_none() {
        println!("  Skipped. Open MUR Hub → Settings → Models to configure models.");
    } else {
        let mut config = crate::store::config::load_config()?;
        crate::model_setup::apply(&plan, &mut config);
        crate::store::config::save_config(&config)?;
        println!("  ✓ {}", plan.summary);
    }
```

- [ ] **Step 2: Delete the dead helpers** listed above. Run `cargo clippy -p mur-core -- -D warnings 2>&1 | head -30` and remove anything it flags as newly dead (e.g. imports of `init_local::select_local_llm` used only by the deleted mode-3 arm — keep `init_local` itself; other callers exist).

- [ ] **Step 3: Verify**

Run: `cargo test -p mur-core cmd_init && cargo clippy -p mur-core -- -D warnings`
Expected: `cmd_init_accepts_refresh_discovery` still PASSES; clippy clean. Then a live smoke: `printf '\n\n\n\n\n\n\n\n' | MUR_HOME=$(mktemp -d) cargo run -p mur-core --bin mur -- init 2>&1 | grep -A3 "Model setup"` → shows the two options and applies default without panicking.

- [ ] **Step 4: Commit + PR**

```bash
cargo fmt --all && git add -A && git commit -m "feat(init): collapse Step G model setup to one question via model_setup::recommend"
gh pr create --base feat/hub-unified-model-setup --title "feat: api_key_ref + shared model-setup engine + one-question init (PR1/3)" --body "…link spec…

🤖 Generated with [Claude Code](https://claude.com/claude-code)"
```

---

# PR 2 — Hub Settings › Models one-pager

Branch: `git checkout -b feat/hub-model-slots` (base: PR1 branch).

### Task 7: `model_setup::slots` — get/set with follow heuristic

**Files:**
- Create: `mur-core/src/model_setup/slots.rs` (+ `pub mod slots;` in `model_setup/mod.rs`)

**Interfaces:**
- Consumes: `crate::store::config::{load_config, save_config}`, `ModelRegistry`, Task 1 fields, `RoleEntry` (`mur_common::model`).
- Produces (Task 8 serializes these straight through tauri):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotId { Smart, Search, Ask, Compact, Rollup, Summarize, Reflector, Curator }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlotSelection {
    /// Pick a registry model by ref name — secret ref comes from the entry.
    Registry { ref_name: String },
    /// Pick a detected local model.
    Local { provider: String, model: String, base_url: String, dims: Option<usize> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotView {
    pub provider: String,
    pub model: String,
    pub api_key_ref: Option<String>,
    /// "ready" | "key_missing" | "unset"
    pub health: String,
    /// True when this sub-slot mirrors the smart slot (value equality).
    pub follows_smart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSlotsView {
    pub smart: SlotView,
    pub search: SlotView,
    pub ask: SlotView,
    pub compact: SlotView,
    pub rollup: SlotView,
    pub summarize: Option<String>,
    pub reflector: Option<String>,
    pub curator: Option<String>,
}

pub fn get_slots() -> anyhow::Result<ModelSlotsView>;
pub fn set_slot(slot: SlotId, sel: &SlotSelection) -> anyhow::Result<ModelSlotsView>;
```

Semantics (implement exactly):
- Effective pair of a conversations stage: `ask.backend`/`compact.extractive_backend` override `Some(b)` → `(b.provider, b.model)`; `None` → `("ollama", <legacy model string>)`. Rollup has NO backend field → always `("ollama", rollup.extractive_model)`.
- `follows_smart` = effective pair == `(llm.provider, llm.model)`.
- `set_slot(Smart, sel)`: capture old pair; write `llm.*` (Registry → provider/model/base_url→`openai_url` for openai-compat providers, `api_key_ref` = entry.secret.to_string(); Local → provider/model, `api_key_ref: None`). Then for each of ask/compact whose effective pair == old pair: mirror — Local sel → clear backend override to `None` + set legacy model string; Registry sel → set the per-stage backend override `Some(BackendConfig { provider, model, endpoint: entry.base_url, api_key_env: None, api_key_ref: entry.secret.map(|s| s.to_string()), timeout_secs: None })`. Rollup mirrors ONLY for Local selections (set `rollup.extractive_model`); a cloud smart leaves rollup untouched (local-only stage).
- `set_slot(Ask|Compact, sel)`: same single-stage write as the mirror logic.
- `set_slot(Rollup|Summarize, sel)`: `Local` only — `Registry` returns `Err("this stage runs locally; pick a local model")`. Summarize writes `ask.summarize_model = Some(model)`.
- `set_slot(Search, sel)`: `embedding.provider/model/api_key_ref`; dims from `sel` dims → `fallback_dims_for` → keep existing.
- `set_slot(Reflector|Curator, SlotSelection::Registry{ref_name})`: `reg.roles.insert(<role>, RoleEntry { primary: ref_name, fallback: None, cost_budget_per_day_usd: None, privacy_local_only: false, route_policy: None })` + `reg.save_to(path)`. `Local` → Err (roles are registry-native).
- Health (ponytail: key-check only — live runtime probes are the Model Library's job): cloud slot with `api_key_ref` → `"ready"` if the ref parses+`resolve_blocking().is_ok()` else `"key_missing"`; local slot → `"ready"`; factory-default untouched slot → `"unset"`.

- [ ] **Step 1: Write failing tests** (test mod in `slots.rs`; use `MUR_HOME` tempdir like `first_launch.rs` tests — set `MUR_HOME` to a `TempDir`, guard with a static `Mutex` since env is process-global)

```rust
// Env vars are process-global — serialize the two tests below.
static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn smart_set_mirrors_following_stages_only() {
    let _g = ENV_TEST_LOCK.lock().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe { std::env::set_var("MUR_HOME", tmp.path()) };
    // Default config: ask/compact/rollup all == DEFAULT_LOCAL_LLM_MODEL, llm = anthropic default → nothing follows.
    let v = get_slots().unwrap();
    assert!(!v.ask.follows_smart);
    // Point smart at the same local model conversations use → they follow.
    let sel = SlotSelection::Local {
        provider: "ollama".into(),
        model: mur_common::config::DEFAULT_LOCAL_LLM_MODEL.into(),
        base_url: "http://localhost:11434".into(),
        dims: None,
    };
    let v = set_slot(SlotId::Smart, &sel).unwrap();
    assert!(v.ask.follows_smart && v.compact.follows_smart && v.rollup.follows_smart);
    // Now move smart to another local model → followers move with it.
    let sel2 = SlotSelection::Local {
        provider: "ollama".into(),
        model: "llama3:8b".into(),
        base_url: "http://localhost:11434".into(),
        dims: None,
    };
    let v = set_slot(SlotId::Smart, &sel2).unwrap();
    assert_eq!(v.ask.model, "llama3:8b");
    assert_eq!(v.rollup.model, "llama3:8b");
    unsafe { std::env::remove_var("MUR_HOME") };
}

#[test]
fn rollup_rejects_registry_selection() {
    let _g = ENV_TEST_LOCK.lock().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    unsafe { std::env::set_var("MUR_HOME", tmp.path()) };
    let sel = SlotSelection::Registry { ref_name: "whatever".into() };
    assert!(set_slot(SlotId::Rollup, &sel).is_err());
    unsafe { std::env::remove_var("MUR_HOME") };
}
```

Verify `store::config::load_config` honors `MUR_HOME` (`rg -n "MUR_HOME" mur-core/src/store/config.rs`); if it uses a different override, mirror what `cmd/model.rs` tests do.

- [ ] **Step 2: Run to fail**: `cargo test -p mur-core slots` — FAIL
- [ ] **Step 3: Implement per the semantics block above.** Keep the file under 400 lines; helpers `fn effective_pair(...)`, `fn write_stage(...)` shared by mirror + direct set.
- [ ] **Step 4: Run**: `cargo test -p mur-core slots` — PASS; `cargo clippy -p mur-core -- -D warnings`
- [ ] **Step 5: Commit** — `feat(model-setup): slot get/set with follow-smart heuristic`

### Task 8: Hub tauri commands

**Files:**
- Create: `mur-hub-gui/src-tauri/src/model_slots.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (add `mod model_slots;` next to `mod models_admin;` and register commands in `generate_handler!` after `onboarding::first_launch::replay_onboarding,`)

**Interfaces:**
- Produces (TS side): `invoke("model_slots_get") -> ModelSlotsView`, `invoke("model_slots_set", { slot, sel }) -> ModelSlotsView`.

- [ ] **Step 1: Implement** (thin wrappers, String errors like every other command file):

```rust
//! Settings › Models slot commands — thin wrappers over
//! mur_core::model_setup::slots (single source of truth for slot writes).

use mur_core::model_setup::slots::{ModelSlotsView, SlotId, SlotSelection};

#[tauri::command]
pub fn model_slots_get() -> Result<ModelSlotsView, String> {
    mur_core::model_setup::slots::get_slots().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn model_slots_set(slot: SlotId, sel: SlotSelection) -> Result<ModelSlotsView, String> {
    mur_core::model_setup::slots::set_slot(slot, &sel).map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Verify**: `[ -f mur-hub-gui/ui/dist/index.html ] || (mkdir -p mur-hub-gui/ui/dist && echo '<!doctype html>' > mur-hub-gui/ui/dist/index.html)` then `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` — clean.
- [ ] **Step 3: Commit** (never commit the dist stub) — `feat(hub): model_slots_get/set tauri commands`

### Task 9: UI slot helpers + tests

**Files:**
- Create: `mur-hub-gui/ui/src/components/modelSlots.ts`
- Test: `mur-hub-gui/ui/src/components/modelSlots.test.ts`

**Interfaces:**
- Consumes: `ModelOption` (`./modelPicker`), `DetectedLocalView`-shaped data from `probe_local_providers` (`{ key, name, base_url, models: { model, alias }[] }`).
- Produces:

```ts
export interface SlotOptionGroup { label: string; options: SlotOption[] }
export interface SlotOption { label: string; payload: SlotSelection }
export type SlotSelection =
  | { kind: "registry"; ref_name: string }
  | { kind: "local"; provider: string; model: string; base_url: string; dims: number | null };
export function buildSlotGroups(registry: ModelOption[], local: LocalProvider[]): SlotOptionGroup[];
export function encodeSel(s: SlotSelection): string;   // JSON.stringify — <option value>
export function decodeSel(v: string): SlotSelection;   // JSON.parse
```

`buildSlotGroups`: one group per cloud provider from the registry (label = provider name capitalized, option label = ref_name), then one group per detected local provider (label = `${name} (local)`, payload kind "local", provider `key === "omlx" ? "omlx" : "ollama"` — mirror what `probe_local_providers` keys are; check `models_admin.rs` `detect` mapping and copy its key strings).

- [ ] **Step 1: Write the failing test** (`modelSlots.test.ts`, mirror `modelPicker.test.ts` imports/style):

```ts
import { describe, expect, it } from "vitest";
import { buildSlotGroups, decodeSel, encodeSel } from "./modelSlots";

const reg = [
  { ref_name: "anthropic_opus", provider: "anthropic", model: "claude-opus-4-6", tier: null, input_cost: null, output_cost: null, context_window: null, capabilities: [] },
];
const local = [
  { key: "ollama", name: "Ollama", base_url: "http://localhost:11434", models: [{ model: "qwen3.5:4b", alias: "ollama_qwen35_4b", input_cost: null, output_cost: null, context_window: null }] },
];

describe("buildSlotGroups", () => {
  it("groups registry by provider then local providers", () => {
    const g = buildSlotGroups(reg as never, local as never);
    expect(g[0].label).toBe("Anthropic");
    expect(g[0].options[0].payload).toEqual({ kind: "registry", ref_name: "anthropic_opus" });
    expect(g[1].label).toBe("Ollama (local)");
    expect(g[1].options[0].payload).toMatchObject({ kind: "local", model: "qwen3.5:4b" });
  });
  it("encode/decode round-trips", () => {
    const s = { kind: "registry", ref_name: "x" } as const;
    expect(decodeSel(encodeSel(s))).toEqual(s);
  });
});
```

- [ ] **Step 2: Run to fail**: `cd mur-hub-gui/ui && npx vitest run src/components/modelSlots.test.ts` — FAIL
- [ ] **Step 3: Implement** (~40 lines, pure). 
- [ ] **Step 4: Run** — PASS. **Step 5: Commit** — `feat(hub-ui): modelSlots option-group helpers`

### Task 10: ModelsSettings one-pager + i18n

**Files:**
- Modify: `mur-hub-gui/ui/src/components/settings/ModelsSettings.tsx` (rewrite)
- Modify: `mur-hub-gui/ui/src/i18n/en.ts`, `mur-hub-gui/ui/src/i18n/zh-TW.ts`
- Modify: `mur-hub-gui/ui/src/styles/components/modal.css` (or the settings CSS file the section already uses — follow existing class conventions)

**Interfaces:**
- Consumes: `model_slots_get`/`model_slots_set` (Task 8), `buildSlotGroups` (Task 9), existing `list_models`, `probe_local_providers`, `RegistryList`, `ModelLibrary`.

- [ ] **Step 1: i18n keys** (en shown; zh-TW mirrors — write BOTH):

```ts
  "settings.slots.smart": "Smart model",
  "settings.slots.smartHint": "Learning and background conversation stages follow this pick unless overridden below.",
  "settings.slots.search": "Search model",
  "settings.slots.brain": "Agent default brain",
  "settings.slots.advanced": "Advanced overrides",
  "settings.slots.follows": "follows Smart model",
  "settings.slots.localOnly": "local only",
  "settings.slots.ready": "ready",
  "settings.slots.keyMissing": "key missing — reconnect the provider in the Model Library",
  "settings.slots.unset": "not configured",
  "settings.slots.pick": "Choose a model…",
```

zh-TW: `"settings.slots.smart": "智能模型"`, `"settings.slots.search": "搜尋模型"`, `"settings.slots.brain": "Agent 預設大腦"`, `"settings.slots.advanced": "進階覆寫"`, `"settings.slots.follows": "跟隨智能模型"`, `"settings.slots.localOnly": "僅限本地"`, `"settings.slots.ready": "可用"`, `"settings.slots.keyMissing": "金鑰缺失 — 請到 Model Library 重新連接"`, `"settings.slots.unset": "尚未設定"`, `"settings.slots.pick": "選擇模型…"`, smartHint = `"學習與背景對話階段預設跟隨此選擇,可在下方逐項覆寫。"`.

- [ ] **Step 2: Rewrite `ModelsSettings.tsx`.** Structure (native `<select>` with `<optgroup>` — platform feature, keyboard/AX free; `<details>` for the accordion):

```tsx
export function ModelsSettings() {
  const { t } = useT();
  const [slots, setSlots] = useState<ModelSlotsView | null>(null);
  const [groups, setGroups] = useState<SlotOptionGroup[]>([]);
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const refresh = useCallback(() => {
    invoke<ModelSlotsView>("model_slots_get").then(setSlots).catch(() => {});
    Promise.all([
      invoke<ModelOption[]>("list_models").catch(() => [] as ModelOption[]),
      invoke<LocalProvider[]>("probe_local_providers").catch(() => [] as LocalProvider[]),
    ]).then(([reg, local]) => setGroups(buildSlotGroups(reg, local)));
  }, []);
  useEffect(refresh, [refresh]);
  useEffect(() => { if (!libraryOpen) refresh(); }, [libraryOpen, refresh]);

  const setSlot = (slot: string) => (e: React.ChangeEvent<HTMLSelectElement>) => {
    if (!e.target.value) return;
    invoke<ModelSlotsView>("model_slots_set", { slot, sel: decodeSel(e.target.value) })
      .then((v) => { setErr(null); setSlots(v); })
      .catch((x) => setErr(String(x)));
  };

  const row = (labelKey: string, slot: string, view: SlotView, opts?: { localOnly?: boolean }) => (
    <div className="settings-row">
      <span className="settings-row__label">
        {t(labelKey)}
        {opts?.localOnly && <em className="slot-tag">{t("settings.slots.localOnly")}</em>}
      </span>
      <select className="slot-select" value="" onChange={setSlot(slot)} aria-label={t(labelKey)}>
        <option value="">{`${view.provider}/${view.model}`}</option>
        {groups.map((g) => (
          <optgroup key={g.label} label={g.label}>
            {g.options.map((o) => (
              <option key={o.label} value={encodeSel(o.payload)}>{o.label}</option>
            ))}
          </optgroup>
        ))}
      </select>
      <span className={`slot-health slot-health--${view.health}`}>
        {t(`settings.slots.${view.health === "key_missing" ? "keyMissing" : view.health}`)}
      </span>
    </div>
  );

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.models")}</h3>
      {slots && (
        <>
          {row("settings.slots.smart", "smart", slots.smart)}
          <p className="settings-hint">{t("settings.slots.smartHint")}</p>
          {row("settings.slots.search", "search", slots.search)}
          {/* Agent default brain: keep the existing nudge_status display row */}
          <details className="slot-advanced">
            <summary>{t("settings.slots.advanced")}</summary>
            {row("conv.ask", "ask", slots.ask)}
            {row("conv.compact", "compact", slots.compact)}
            {row("conv.rollup", "rollup", slots.rollup, { localOnly: true })}
            {/* reflector / curator rows: registry options only — filter groups to kind "registry" */}
          </details>
          {err && <p className="settings-hint slot-error">{err}</p>}
        </>
      )}
      <div className="settings-row">
        <button className="toolbar-btn" onClick={() => setLibraryOpen(true)}>{t("settings.openLibrary")}</button>
      </div>
      <ModelLibrary open={libraryOpen} onClose={() => setLibraryOpen(false)} />
    </section>
  );
}
```

Keep the existing default-brain row and `RegistryList` if it fits below; add i18n keys `conv.ask`/`conv.compact`/`conv.rollup` ("Ask model"/"Compact model"/"Rollup model"; zh-TW 「對話 Ask 模型」「Compact 摘要模型」「Rollup 彙整模型」). Sub-slot rows that `follows_smart` show `t("settings.slots.follows")` instead of a health badge. Add `slot-select`, `slot-health--*` (ready=green, key_missing=amber, unset=muted), `slot-advanced`, `slot-tag` classes in CSS following the file's existing tokens.

- [ ] **Step 3: Verify**: `cd mur-hub-gui/ui && npx tsc --noEmit && npx vitest run` — clean; `npm run build` succeeds.
- [ ] **Step 4: Manual smoke** (deferred to PR checklist if no display): open Hub → Settings → Models; change Smart → conversations rows follow; pick cloud model with missing key → amber badge.
- [ ] **Step 5: Commit + PR 2** — `feat(hub-ui): Settings › Models one-pager with slot pickers` → `gh pr create --base feat/model-setup-keyref --title "feat: Hub Settings Models one-pager (PR2/3)"`.

---

# PR 3 — First-run wizard model step

Branch: `git checkout -b feat/hub-model-wizard` (base: PR2 branch).

### Task 11: Wizard status/preview/apply commands

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/model_slots.rs` (append)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (register 3 commands)

**Interfaces:**
- Produces: `invoke("model_setup_status") -> { needs_setup: boolean }`, `invoke("model_setup_preview") -> { summary: string, has_plan: boolean }`, `invoke("model_setup_apply_recommended") -> { summary: string }`.
- Consumes: `mur_core::model_setup::*` (Task 5), `mur_core::discovery::run_all()` (async), `mur_core::store::config`.

- [ ] **Step 1: Implement**

```rust
#[derive(serde::Serialize)]
pub struct SetupStatus { pub needs_setup: bool }

#[tauri::command]
pub fn model_setup_status() -> Result<SetupStatus, String> {
    let cfg = mur_core::store::config::load_config().map_err(|e| e.to_string())?;
    let reg_empty = mur_common::model::ModelRegistry::default_path()
        .and_then(|p| mur_common::model::ModelRegistry::load_from(&p))
        .map(|r| r.models.is_empty())
        .unwrap_or(true);
    Ok(SetupStatus {
        needs_setup: reg_empty && mur_core::model_setup::is_factory_default_models(&cfg),
    })
}

#[derive(serde::Serialize)]
pub struct SetupPreview { pub summary: String, pub has_plan: bool }

async fn build_plan() -> Result<mur_core::model_setup::ModelSetupPlan, String> {
    let discovered = mur_core::discovery::run_all().await.unwrap_or_default();
    let mut keys = mur_core::model_setup::probe_env_keys();
    if let Ok(p) = mur_common::model::ModelRegistry::default_path()
        && let Ok(reg) = mur_common::model::ModelRegistry::load_from(&p)
    {
        keys.extend(mur_core::model_setup::keychain_key_sources(&reg));
    }
    Ok(mur_core::model_setup::recommend(&discovered, &keys))
}

#[tauri::command]
pub async fn model_setup_preview() -> Result<SetupPreview, String> {
    let plan = build_plan().await?;
    Ok(SetupPreview { summary: plan.summary.clone(), has_plan: plan.smart.is_some() })
}

#[tauri::command]
pub async fn model_setup_apply_recommended() -> Result<SetupPreview, String> {
    let plan = build_plan().await?;
    if plan.smart.is_some() {
        let mut cfg = mur_core::store::config::load_config().map_err(|e| e.to_string())?;
        mur_core::model_setup::apply(&plan, &mut cfg);
        mur_core::store::config::save_config(&cfg).map_err(|e| e.to_string())?;
    }
    Ok(SetupPreview { summary: plan.summary.clone(), has_plan: plan.smart.is_some() })
}
```

Check `discovery::run_all` visibility/signature (`rg -n "pub async fn run_all" mur-core/src/discovery/`); if it returns `Result<Vec<DiscoveredModel>>`, the `.unwrap_or_default()` stands.

- [ ] **Step 2: Verify**: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` + `cargo test -p mur-core is_factory_default` — clean/PASS.
- [ ] **Step 3: Commit** — `feat(hub): model wizard status/preview/apply commands`

### Task 12: `ModelSetupWizard` + DashboardApp trigger + i18n

**Files:**
- Create: `mur-hub-gui/ui/src/components/ModelSetupWizard.tsx`
- Modify: `mur-hub-gui/ui/src/components/DashboardApp.tsx` (first-launch effect, ~line 586)
- Modify: `mur-hub-gui/ui/src/i18n/en.ts`, `zh-TW.ts`
- Modify: wizard CSS file (`ui/src/styles/components/wizard.css`)

**Interfaces:**
- Consumes: Task 11 commands, `ModelLibrary` component, `SettingsModal` opener already present in DashboardApp (find its state setter with `rg -n "SettingsModal|settingsOpen" DashboardApp.tsx`).

- [ ] **Step 1: i18n** (en / zh-TW pairs):

```ts
  "wizard.models.title": "Set up models",
  "wizard.models.detecting": "Detecting local runtimes and connected providers…",
  "wizard.models.apply": "Apply recommended setup",
  "wizard.models.customize": "Customize…",
  "wizard.models.skip": "Skip for now",
  "wizard.models.connect": "Connect a provider",
  "wizard.models.done": "Models configured:",
  "wizard.models.hint": "You can change this anytime in Settings → Models.",
```

zh-TW: 「設定模型」「正在偵測本地執行環境與已連接的供應商…」「套用建議配置」「自訂…」「暫時跳過」「連接供應商」「模型已設定:」「之後隨時可到 設定 → Models 調整。」

- [ ] **Step 2: Component**

```tsx
/** First-run model setup: one screen, three exits (apply / customize / skip).
 *  Shown only when model_setup_status says nothing usable is configured. */
export function ModelSetupWizard({ open, onClose, onCustomize }: {
  open: boolean; onClose: () => void; onCustomize: () => void;
}) {
  const { t } = useT();
  const [summary, setSummary] = useState<string | null>(null);
  const [hasPlan, setHasPlan] = useState(false);
  const [phase, setPhase] = useState<"detect" | "ready" | "applying" | "done">("detect");
  const [libraryOpen, setLibraryOpen] = useState(false);

  const preview = useCallback(() => {
    setPhase("detect");
    invoke<{ summary: string; has_plan: boolean }>("model_setup_preview")
      .then((p) => { setSummary(p.summary); setHasPlan(p.has_plan); setPhase("ready"); })
      .catch(() => { setSummary(null); setHasPlan(false); setPhase("ready"); });
  }, []);
  useEffect(() => { if (open) preview(); }, [open, preview]);
  useEffect(() => { if (!libraryOpen && open) preview(); }, [libraryOpen]); // re-probe after connecting

  if (!open) return null;
  return (
    <div className="wizard-overlay" role="dialog" aria-modal="true" aria-label={t("wizard.models.title")}>
      <div className="wizard-card">
        <h2>{t("wizard.models.title")}</h2>
        {phase === "detect" && <p>{t("wizard.models.detecting")}</p>}
        {phase !== "detect" && <p className="wizard-summary">{summary}</p>}
        {phase === "done" ? (
          <>
            <p>{t("wizard.models.done")} {summary}</p>
            <button className="toolbar-btn" onClick={onClose}>OK</button>
          </>
        ) : (
          <div className="wizard-actions">
            {hasPlan ? (
              <button className="toolbar-btn toolbar-btn--primary" disabled={phase !== "ready"}
                onClick={() => {
                  setPhase("applying");
                  invoke<{ summary: string }>("model_setup_apply_recommended")
                    .then((p) => { setSummary(p.summary); setPhase("done"); })
                    .catch(() => setPhase("ready"));
                }}>
                {t("wizard.models.apply")}
              </button>
            ) : (
              <button className="toolbar-btn toolbar-btn--primary" onClick={() => setLibraryOpen(true)}>
                {t("wizard.models.connect")}
              </button>
            )}
            <button className="toolbar-btn" onClick={() => { onClose(); onCustomize(); }}>
              {t("wizard.models.customize")}
            </button>
            <button className="toolbar-btn" onClick={onClose}>{t("wizard.models.skip")}</button>
          </div>
        )}
        <p className="settings-hint">{t("wizard.models.hint")}</p>
        <ModelLibrary open={libraryOpen} onClose={() => setLibraryOpen(false)} />
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Trigger in DashboardApp** — extend the existing first-launch effect (line ~586):

```tsx
      if (status.is_first_launch) {
        invoke("mark_first_launch_done").catch(() => {});
        invoke<{ needs_setup: boolean }>("model_setup_status")
          .then((s) => { if (s.needs_setup) setShowModelWizard(true); })
          .catch(() => {});
      }
```

Add `const [showModelWizard, setShowModelWizard] = useState(false);` and render `<ModelSetupWizard open={showModelWizard} onClose={() => setShowModelWizard(false)} onCustomize={() => { /* open SettingsModal on models tab — reuse existing settings-open state */ }} />`. Wire `onCustomize` to the existing settings-modal opener (find and pass its tab param if supported; otherwise just open settings).

- [ ] **Step 4: Verify**: `cd mur-hub-gui/ui && npx tsc --noEmit && npm run build` — clean. Manual: `MUR_HOME=$(mktemp -d) + rm ~/.mur/.hub_onboarded` equivalent → wizard appears; with a configured `~/.mur` → never appears.
- [ ] **Step 5: Commit + PR 3** — `feat(hub-ui): first-run model setup wizard` → `gh pr create --base feat/hub-model-slots --title "feat: Hub first-run model wizard (PR3/3)"`.

### Task 13: Docs touch-up

**Files:**
- Modify: `README.md` (if it documents init's model prompts — `rg -n "Model setup|embedding" README.md`), `docs/architecture/runtime-overview.md` (same grep)

- [ ] **Step 1:** Update any description of the multi-step init model flow to the one-question flow + Hub Settings › Models. One paragraph each, no new sections.
- [ ] **Step 2:** Note in PR3 body: app.mur.run docs (`mur-server/dashboard/docs-content/`) need a matching update — separate repo, follow-up.
- [ ] **Step 3:** Commit — `docs: one-question init + Hub model settings`

---

## Self-review notes (already applied)

- Spec §1–§5 → Tasks 1–6 (PR1), §2 → 7–10 (PR2), §3 → 11–12 (PR3), i18n/branding → Tasks 10/12, testing §7 → per-task TDD steps.
- Rollup/summarize are local-only because `RollupConfig`/`summarize_model` have no per-stage `BackendConfig` override — enforced in `set_slot`, labeled in UI (`localOnly` tag). This is a deliberate narrowing vs. the spec's generic "advanced overrides"; adding rollup cloud support would require a config schema change (out of scope).
- `keychain_key_sources` resolves each secret once (keychain prompt-free for own items); if that proves slow on cold keychain, drop the `resolve_blocking` check and let health badges catch dead refs.

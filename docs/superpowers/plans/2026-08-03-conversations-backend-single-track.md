# Conversations Backend Single Track — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `conversations.{ask,compact,rollup}` resolve their chat model through one mechanism — an optional `BackendConfig` that, when absent, inherits the smart slot — instead of a legacy model-name string that silently fabricates an Ollama identity.

**Architecture:** Delete the legacy `model` / `ollama_endpoint` fields from the three stage configs and replace the four `synthesize_*_backend()` fabricators with `effective_backend(&llm)` resolvers that fall back to `LlmConfig::to_backend_config()`. A pure text→text migration converts existing config files at load time, before the struct ever sees them. Writers (setup wizard, Hub slot setter) stop emitting bare model names. `mur chat doctor` gains a per-stage table plus a live probe of each unique endpoint.

**Tech Stack:** Rust 2024, `serde` + `serde_yaml_ng`, `cargo nextest`, `wiremock` for HTTP stubs.

**Spec:** `docs/superpowers/specs/2026-08-03-conversations-backend-single-track-design.md`

## Global Constraints

- Test runner is **`cargo nextest run`**, not `cargo test` — plain `cargo test --workspace` fails ~7 tests spuriously in this repo.
- `mur-core` needs `ORT_STRATEGY=download` and `MUR_WEB_DIST=$HOME/Projects/mur-web/dist` exported, or it fails to link/compile. `mur-common` needs neither.
- A full `cargo nextest run -p mur-core` hits a pre-existing debug stack overflow in ~7 `bin/mur` CLI-parse tests. Use `RUST_MIN_STACK=33554432` when running the whole crate.
- YAML writes use **temp file + rename** for atomicity (`store/yaml.rs` convention).
- Rust edition 2024 — `let` chains are stable.
- No hardcoded values: use the existing constants (`DEFAULT_LOCAL_LLM_MODEL = "qwen3.5:4b"`, `config.rs:4`) rather than repeating literals.
- Single source file ≤ 800 lines. `mur-common/src/config.rs` is already 2396 and `mur-core/src/cmd/conversations_cmd.rs` is 1592 — do not add net lines to either. New code goes in new modules.
- Brand name in user-visible strings is uppercase **MUR**.

---

## Task → PR mapping

The spec requires the schema change and the migration to ship in the **same
release** — a release with the fields deleted but no migration reopens the
data-loss window §3 describes.

| PR | Tasks | Deliverable |
| --- | --- | --- |
| 1 | 1–6 | Single-track schema, resolvers, migration, wired into both load paths. **Must not be split.** |
| 2 | 7 | Setup wizard and Hub slot setter write real backends |
| 3 | 8 | doctor lists and probes each stage |
| 4 | 9 | Workspace verification and docs |

---

### Deviation from the spec — read before starting

The spec's §3 mentions a `mur migrate --conversations --dry-run` preview command. **This plan drops it.** Because migration runs automatically during `load_config()`, by the time any `mur` subcommand parses its arguments the config is already migrated, so the preview would always print "nothing to do". The one-line notice printed by the automatic migration plus the new doctor table (Task 8) cover the need. Nothing else in the spec changes.

---

### Task 1: Rollup gains backend override fields

Purely additive — legacy fields stay, nothing else changes yet. This is the field that `rollup.rs:176-179` explicitly notes as missing.

**Files:**
- Modify: `mur-common/src/config.rs:1055-1100` (`RollupConfig` struct + its `Default` impl)
- Test: `mur-common/src/config.rs` (`mod tests`, append)

**Interfaces:**
- Produces: `RollupConfig.extractive_backend: Option<BackendConfig>`, `RollupConfig.abstractive_backend: Option<BackendConfig>` — consumed by Tasks 2, 4, 6, 7, 8.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `mur-common/src/config.rs`:

```rust
    #[test]
    fn rollup_config_accepts_backend_overrides() {
        let yaml = r#"
enabled: true
extractive_backend:
  provider: openai
  model: Qwen3.5-4B-MLX-4bit
  endpoint: http://127.0.0.1:8000/v1
"#;
        let c: RollupConfig = serde_yaml_ng::from_str(yaml).expect("parses");
        let b = c.extractive_backend.expect("override present");
        assert_eq!(b.provider, "openai");
        assert_eq!(b.model, "Qwen3.5-4B-MLX-4bit");
        assert_eq!(b.endpoint.as_deref(), Some("http://127.0.0.1:8000/v1"));
        assert!(c.abstractive_backend.is_none());
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p mur-common rollup_config_accepts_backend_overrides
```

Expected: FAIL — `no field 'extractive_backend' on type 'RollupConfig'`.

- [ ] **Step 3: Add the fields**

In `mur-common/src/config.rs`, inside `pub struct RollupConfig`, after `pub ollama_endpoint: String,`:

```rust
    /// Per-stage backend override for the extractive stage.
    /// None = inherit the smart slot (`config.llm`).
    #[serde(default)]
    pub extractive_backend: Option<BackendConfig>,
    /// Per-stage backend override for the abstractive stage.
    /// None = inherit the smart slot (`config.llm`).
    #[serde(default)]
    pub abstractive_backend: Option<BackendConfig>,
```

And in `impl Default for RollupConfig`, after `ollama_endpoint: compact_default_ollama_endpoint(),`:

```rust
            extractive_backend: None,
            abstractive_backend: None,
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo nextest run -p mur-common rollup_config_accepts_backend_overrides
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(config): give rollup the per-stage backend override it never had"
```

---

### Task 2: `effective_backend` resolvers replace the Ollama fabricators

**Files:**
- Modify: `mur-common/src/config.rs` — `impl AskConfig` (`:775-817`), `impl CompactConfig` (`:970-1007`), new `impl RollupConfig`
- Test: `mur-common/src/config.rs` (`mod tests`, append)

**Interfaces:**
- Consumes: `RollupConfig.{extractive,abstractive}_backend` from Task 1; the existing `LlmConfig::to_backend_config()` (`config.rs:500-514`), which already maps any unknown provider carrying an `openai_url` (omlx, mlx) to `"openai"`.
- Produces, all taking `&LlmConfig` and returning `BackendConfig`:
  - `AskConfig::effective_backend`, `AskConfig::effective_rewriter_backend`
  - `CompactConfig::effective_extractive_backend`, `CompactConfig::effective_abstractive_backend`
  - `RollupConfig::effective_extractive_backend`, `RollupConfig::effective_abstractive_backend`

  The four `synthesize_*` functions remain in place for now; Task 4 deletes them along with their callers.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests`:

```rust
    fn omlx_llm() -> LlmConfig {
        LlmConfig {
            provider: "omlx".into(),
            model: "Qwen3.5-4B-MLX-4bit".into(),
            api_key_env: None,
            api_key_ref: Some("env:OMLX_API_KEY".into()),
            openai_url: Some("http://127.0.0.1:8000/v1".into()),
        }
    }

    #[test]
    fn ask_without_override_inherits_smart_slot_and_maps_omlx_to_openai() {
        let ask = AskConfig::default();
        let b = ask.effective_backend(&omlx_llm());
        assert_eq!(b.provider, "openai");
        assert_eq!(b.model, "Qwen3.5-4B-MLX-4bit");
        assert_eq!(b.endpoint.as_deref(), Some("http://127.0.0.1:8000/v1"));
        assert_eq!(b.api_key_ref.as_deref(), Some("env:OMLX_API_KEY"));
        // stage timeout is baked in, not left to the factory's 120s default
        assert_eq!(b.timeout_secs, Some(ask.timeout_secs as u64));
    }

    #[test]
    fn ask_rewriter_inherits_its_own_shorter_timeout_not_the_answer_one() {
        let ask = AskConfig::default();
        let b = ask.effective_rewriter_backend(&omlx_llm());
        assert_eq!(b.timeout_secs, Some(ask.rewriter_timeout_secs as u64));
        assert_ne!(b.timeout_secs, Some(ask.timeout_secs as u64));
    }

    #[test]
    fn explicit_override_wins_over_the_smart_slot() {
        let mut ask = AskConfig::default();
        ask.backend = Some(BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: Some(42),
        });
        let b = ask.effective_backend(&omlx_llm());
        assert_eq!(b.provider, "anthropic");
        assert_eq!(b.timeout_secs, Some(42));
    }

    #[test]
    fn compact_and_rollup_inherit_smart_slot_with_the_120s_budget() {
        let llm = omlx_llm();
        for b in [
            CompactConfig::default().effective_extractive_backend(&llm),
            CompactConfig::default().effective_abstractive_backend(&llm),
            RollupConfig::default().effective_extractive_backend(&llm),
            RollupConfig::default().effective_abstractive_backend(&llm),
        ] {
            assert_eq!(b.provider, "openai");
            assert_eq!(b.endpoint.as_deref(), Some("http://127.0.0.1:8000/v1"));
            assert_eq!(b.timeout_secs, Some(120));
        }
    }

    #[test]
    fn rollup_override_is_honored() {
        let mut r = RollupConfig::default();
        r.abstractive_backend = Some(BackendConfig {
            provider: "ollama".into(),
            model: "qwen3:4b".into(),
            endpoint: Some("http://box.local:11434".into()),
            api_key_env: None,
            api_key_ref: None,
            timeout_secs: None,
        });
        let b = r.effective_abstractive_backend(&omlx_llm());
        assert_eq!(b.provider, "ollama");
        assert_eq!(b.endpoint.as_deref(), Some("http://box.local:11434"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run -p mur-common effective_
```

Expected: FAIL — `no method named 'effective_backend'`.

- [ ] **Step 3: Implement the resolvers**

Fix the deliberate typo first: in the test written above, rename `abstractive_backend_or_inherit_check` → `effective_abstractive_backend`.

Add to `impl AskConfig` in `mur-common/src/config.rs`:

```rust
    /// Effective backend for answer generation. An explicit per-stage
    /// override wins; otherwise the stage inherits the smart slot
    /// (`config.llm`) with this stage's own timeout baked in, so a slow
    /// backend cannot silently fall back to the factory's 120s default.
    pub fn effective_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.backend.clone().unwrap_or_else(|| BackendConfig {
            timeout_secs: Some(self.timeout_secs as u64),
            ..llm.to_backend_config()
        })
    }

    /// Effective backend for the query rewriter. Deliberately does NOT fall
    /// through to `effective_backend`: the rewriter's output is small and
    /// falling back to the raw question on timeout is non-fatal, so it keeps
    /// the much tighter `rewriter_timeout_secs` budget.
    pub fn effective_rewriter_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.rewriter_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(self.rewriter_timeout_secs as u64),
                ..llm.to_backend_config()
            })
    }
```

Add to `impl CompactConfig`:

```rust
    /// Effective backend for the extractive stage. Override wins; otherwise
    /// inherit the smart slot. CompactConfig has no per-stage timeout field,
    /// so inheritance bakes the same conservative 120s the fabricated Ollama
    /// config used.
    pub fn effective_extractive_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.extractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(120),
                ..llm.to_backend_config()
            })
    }

    /// Effective backend for the abstractive stage. See
    /// `effective_extractive_backend` for the timeout rationale.
    pub fn effective_abstractive_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.abstractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(120),
                ..llm.to_backend_config()
            })
    }
```

Add a new `impl RollupConfig` block immediately after `impl Default for RollupConfig`:

```rust
impl RollupConfig {
    /// Effective backend for the extractive stage. Override wins; otherwise
    /// inherit the smart slot with the same 120s budget the previously
    /// hardcoded inline config used (`summarize/rollup.rs`).
    pub fn effective_extractive_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.extractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(120),
                ..llm.to_backend_config()
            })
    }

    /// Effective backend for the abstractive stage.
    pub fn effective_abstractive_backend(&self, llm: &LlmConfig) -> BackendConfig {
        self.abstractive_backend
            .clone()
            .unwrap_or_else(|| BackendConfig {
                timeout_secs: Some(120),
                ..llm.to_backend_config()
            })
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p mur-common effective_ && cargo nextest run -p mur-common config::
```

Expected: PASS, and the existing 67 `config::` tests still pass.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(config): resolve conversation stages against the smart slot"
```

---

### Task 3: Migration — a pure text→text function

Independent of the struct, so it keeps working after Task 4 deletes the fields. Not wired into anything yet.

**Files:**
- Create: `mur-common/src/config_migrate.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod config_migrate;`)

**Interfaces:**
- Produces: `pub fn migrate_conversations_yaml(text: &str) -> Option<String>` — returns `Some(new_yaml)` when legacy keys were found and converted, `None` when there was nothing to do. Consumed by Task 5.

- [ ] **Step 1: Write the failing tests**

Create `mur-common/src/config_migrate.rs` with only the test module for now:

```rust
//! One-shot migration of the legacy conversations model fields.
//!
//! `conversations.{ask,compact,rollup}` used to carry a bare model-name
//! string plus an `ollama_endpoint`, and resolution fabricated an Ollama
//! backend from them. This converts those into explicit `BackendConfig`
//! overrides so the fields can be deleted.
//!
//! Operates on raw YAML text, NOT on the typed `Config`: by the time this
//! runs the struct no longer has the legacy fields, so a typed load would
//! silently drop exactly the values we need to read.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_stage_migrates_to_inherit() {
        let yaml = "\
conversations:
  ask:
    model: qwen3.5:4b
    ollama_endpoint: http://localhost:11434
    timeout_secs: 120
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        assert!(!out.contains("ollama_endpoint"), "legacy endpoint removed: {out}");
        assert!(!out.contains("model: qwen3.5:4b"), "legacy model removed: {out}");
        assert!(!out.contains("backend:"), "no pin written: {out}");
        assert!(out.contains("timeout_secs: 120"), "unrelated keys kept: {out}");
    }

    #[test]
    fn customized_model_migrates_to_a_pinned_ollama_backend() {
        let yaml = "\
conversations:
  ask:
    model: llama3:70b
    ollama_endpoint: http://localhost:11434
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let b = &v["conversations"]["ask"]["backend"];
        assert_eq!(b["provider"].as_str(), Some("ollama"));
        assert_eq!(b["model"].as_str(), Some("llama3:70b"));
        assert_eq!(b["endpoint"].as_str(), Some("http://localhost:11434"));
    }

    #[test]
    fn default_model_at_a_custom_endpoint_is_pinned_not_inherited() {
        let yaml = "\
conversations:
  ask:
    model: qwen3.5:4b
    ollama_endpoint: http://box.local:11434
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let b = &v["conversations"]["ask"]["backend"];
        assert_eq!(b["endpoint"].as_str(), Some("http://box.local:11434"));
        assert_eq!(b["model"].as_str(), Some("qwen3.5:4b"));
    }

    #[test]
    fn an_existing_override_is_left_alone_and_its_legacy_siblings_dropped() {
        let yaml = "\
conversations:
  ask:
    model: llama3:70b
    ollama_endpoint: http://localhost:11434
    backend:
      provider: openai
      model: Qwen3.5-4B-MLX-4bit
      endpoint: http://127.0.0.1:8000/v1
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(v["conversations"]["ask"]["backend"]["provider"].as_str(), Some("openai"));
        assert!(v["conversations"]["ask"]["model"].is_null());
        assert!(v["conversations"]["ask"]["ollama_endpoint"].is_null());
    }

    #[test]
    fn compacts_two_models_share_one_endpoint() {
        let yaml = "\
conversations:
  compact:
    extractive_model: llama3:70b
    abstractive_model: qwen3.5:4b
    ollama_endpoint: http://box.local:11434
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let c = &v["conversations"]["compact"];
        assert_eq!(c["extractive_backend"]["model"].as_str(), Some("llama3:70b"));
        assert_eq!(c["extractive_backend"]["endpoint"].as_str(), Some("http://box.local:11434"));
        // default model name, but the endpoint was customized → still pinned
        assert_eq!(c["abstractive_backend"]["endpoint"].as_str(), Some("http://box.local:11434"));
    }

    #[test]
    fn rollup_migrates_too() {
        let yaml = "\
conversations:
  rollup:
    extractive_model: llama3:70b
    abstractive_model: llama3:70b
    ollama_endpoint: http://localhost:11434
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(v["conversations"]["rollup"]["extractive_backend"]["model"].as_str(), Some("llama3:70b"));
        assert_eq!(v["conversations"]["rollup"]["abstractive_backend"]["model"].as_str(), Some("llama3:70b"));
    }

    #[test]
    fn is_idempotent() {
        let yaml = "\
conversations:
  ask:
    model: llama3:70b
    ollama_endpoint: http://localhost:11434
";
        let once = migrate_conversations_yaml(yaml).expect("migrates");
        assert!(migrate_conversations_yaml(&once).is_none(), "second pass must be a no-op");
    }

    #[test]
    fn a_config_without_legacy_keys_is_untouched() {
        assert!(migrate_conversations_yaml("skills:\n  max_skills_in_prompt: 5\n").is_none());
        assert!(migrate_conversations_yaml("").is_none());
    }

    #[test]
    fn unparseable_yaml_is_left_alone_rather_than_destroyed() {
        assert!(migrate_conversations_yaml("conversations: [unclosed\n").is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod config_migrate;` to `mur-common/src/lib.rs`, then:

```bash
cargo nextest run -p mur-common config_migrate
```

Expected: FAIL to compile — `cannot find function 'migrate_conversations_yaml'`.

- [ ] **Step 3: Implement the migration**

Prepend to `mur-common/src/config_migrate.rs`, above the test module:

```rust
use serde_yaml_ng::{Mapping, Value};

use crate::config::DEFAULT_LOCAL_LLM_MODEL;

const DEFAULT_OLLAMA_ENDPOINT: &str = "http://localhost:11434";

/// One legacy model field and the override field it becomes.
struct StageField {
    legacy_model: &'static str,
    backend_key: &'static str,
}

/// Every (stage, legacy model field → override field) pair. `compact` and
/// `rollup` each own two models sharing one `ollama_endpoint`.
const STAGES: &[(&str, &[StageField])] = &[
    (
        "ask",
        &[StageField { legacy_model: "model", backend_key: "backend" }],
    ),
    (
        "compact",
        &[
            StageField { legacy_model: "extractive_model", backend_key: "extractive_backend" },
            StageField { legacy_model: "abstractive_model", backend_key: "abstractive_backend" },
        ],
    ),
    (
        "rollup",
        &[
            StageField { legacy_model: "extractive_model", backend_key: "extractive_backend" },
            StageField { legacy_model: "abstractive_model", backend_key: "abstractive_backend" },
        ],
    ),
];

/// Convert legacy `conversations.*` model fields into explicit backend
/// overrides. Returns `None` when there was nothing to migrate — including
/// when the input does not parse, so a syntactically broken config is left
/// untouched rather than replaced.
///
/// A stage counts as *untouched* only when its model name AND its stage's
/// `ollama_endpoint` both still hold the shipped defaults. A default model
/// pointed at a custom endpoint (a remote Ollama box) is a deliberate choice
/// and gets pinned, not inherited away.
pub fn migrate_conversations_yaml(text: &str) -> Option<String> {
    let mut root: Value = serde_yaml_ng::from_str(text).ok()?;
    let conversations = root.get_mut("conversations")?.as_mapping_mut()?;

    let mut changed = false;
    for (stage_name, fields) in STAGES {
        let Some(stage) = conversations
            .get_mut(Value::from(*stage_name))
            .and_then(Value::as_mapping_mut)
        else {
            continue;
        };
        if migrate_stage(stage, fields) {
            changed = true;
        }
    }

    changed.then(|| serde_yaml_ng::to_string(&root).ok()).flatten()
}

/// Returns true when this stage's mapping was modified.
fn migrate_stage(stage: &mut Mapping, fields: &[StageField]) -> bool {
    let endpoint = stage
        .get(Value::from("ollama_endpoint"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    // Nothing legacy present at all → nothing to do.
    let has_legacy =
        endpoint.is_some() || fields.iter().any(|f| stage.contains_key(Value::from(f.legacy_model)));
    if !has_legacy {
        return false;
    }

    let endpoint_is_default =
        endpoint.as_deref().unwrap_or(DEFAULT_OLLAMA_ENDPOINT) == DEFAULT_OLLAMA_ENDPOINT;

    for f in fields {
        let model = stage
            .remove(Value::from(f.legacy_model))
            .as_ref()
            .and_then(Value::as_str)
            .map(str::to_owned);

        // An override the user already wrote always wins — never overwrite it.
        if stage.contains_key(Value::from(f.backend_key))
            && !stage[Value::from(f.backend_key)].is_null()
        {
            continue;
        }

        let Some(model) = model else { continue };
        if model == DEFAULT_LOCAL_LLM_MODEL && endpoint_is_default {
            // Untouched: leave the override absent so the stage inherits smart.
            continue;
        }

        let mut backend = Mapping::new();
        backend.insert(Value::from("provider"), Value::from("ollama"));
        backend.insert(Value::from("model"), Value::from(model));
        backend.insert(
            Value::from("endpoint"),
            Value::from(endpoint.clone().unwrap_or_else(|| DEFAULT_OLLAMA_ENDPOINT.to_string())),
        );
        stage.insert(Value::from(f.backend_key), Value::Mapping(backend));
    }

    stage.remove(Value::from("ollama_endpoint"));
    true
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo nextest run -p mur-common config_migrate
```

Expected: PASS, all 9.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/config_migrate.rs mur-common/src/lib.rs
git commit -m "feat(config): migrate legacy conversation model fields to backend overrides"
```

---

### Task 4: Delete the legacy fields and fix every call site

The tree does not compile between Step 3 and Step 5 of this task. That is expected — the compiler is the checklist.

**Files:**
- Modify: `mur-common/src/config.rs` — remove fields from `AskConfig` (`:729-732`), `CompactConfig` (`:943-949`), `RollupConfig` (`:1075-1080`), their `Default` impls, the four `synthesize_*` functions, `ask_default_model`, `compact_default_model`, `compact_default_ollama_endpoint`, and any test referencing them
- Modify: `mur-core/src/conversations/summarize/mod.rs:97,130`
- Modify: `mur-core/src/conversations/summarize/rollup.rs:180-187,437`
- Modify: `mur-core/src/conversations/backend/adapter.rs:68`
- Modify: `mur-core/src/cmd/conversations_cmd.rs:441-444,1242,1302`
- Modify: `mur-core/src/cmd/skill_evolve.rs:15`

**Interfaces:**
- Consumes: the six `effective_backend` resolvers from Task 2.
- Produces: a `Config` with no legacy conversation model fields. Every consumer must now hold a `&Config` (or at least `&LlmConfig`) rather than only the stage config.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `mur-common/src/config.rs`:

```rust
    #[test]
    fn legacy_conversation_fields_are_gone_from_serialized_output() {
        let cfg = Config::default();
        // Scoped to the conversations block on purpose: `embedding` still
        // carries its own `ollama_endpoint` until Task 5, and asserting over
        // the whole document here would leave a knowingly-red test behind.
        let yaml = serde_yaml_ng::to_string(&cfg.conversations).expect("serializes");
        for key in ["extractive_model", "abstractive_model", "ollama_endpoint"] {
            assert!(
                !yaml.contains(key),
                "legacy key {key} still serialized:\n{yaml}"
            );
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p mur-common legacy_conversation_fields_are_gone
```

Expected: FAIL — the keys are present.

- [ ] **Step 3: Delete the fields and the fabricators**

In `mur-common/src/config.rs`:

- From `AskConfig`: delete `pub model: String,` and `pub ollama_endpoint: String,` with their `#[serde(default = ...)]` attributes. Delete the matching lines from `impl Default for AskConfig`.
- From `CompactConfig`: delete `pub extractive_model`, `pub abstractive_model`, `pub ollama_endpoint` and their `Default` lines.
- From `RollupConfig`: delete `pub extractive_model`, `pub abstractive_model`, `pub ollama_endpoint` and their `Default` lines.
- Delete `synthesize_backend`, `synthesize_rewriter_backend`, `synthesize_extractive_backend`, `synthesize_abstractive_backend`.
- Delete the now-unused `fn ask_default_model`, `fn compact_default_model`, `fn compact_default_ollama_endpoint`.
- Update the doc comments on the four `Option<BackendConfig>` fields: replace "None = synthesize from legacy …" with "None = inherit the smart slot (`config.llm`)."
- Delete or rewrite any test in `mod tests` that references a deleted field. `cargo nextest run -p mur-common` names them.

Keep `fn default_ollama_endpoint` — `EmbeddingConfig` still uses it until Task 5.

- [ ] **Step 4: Fix each call site the compiler names**

`mur-core/src/conversations/summarize/mod.rs:97,130` — these have `cfg: &CompactConfig`. Thread the `LlmConfig` through: change the enclosing function to take `llm: &LlmConfig` and pass it down from the caller, then:

```rust
    let extractive_cfg = cfg.effective_extractive_backend(llm);
    // ...
    let abstractive_cfg = cfg.effective_abstractive_backend(llm);
```

`mur-core/src/conversations/summarize/rollup.rs:180-187` — delete the inline `BackendConfig { provider: "ollama".into(), .. }` literal and the comment above it conceding the gap, replacing both with:

```rust
    let abstractive_cfg = cfg.effective_abstractive_backend(llm);
```

Apply the same substitution at `:437`. Thread `llm: &LlmConfig` in from the caller as above.

`mur-core/src/conversations/backend/adapter.rs:68`:

```rust
    let mut backend_cfg = cfg.conversations.ask.effective_backend(&cfg.llm);
```

`mur-core/src/cmd/conversations_cmd.rs:441-444`:

```rust
    let mut backends = vec![
        cfg.conversations.compact.effective_extractive_backend(&cfg.llm),
        cfg.conversations.compact.effective_abstractive_backend(&cfg.llm),
        cfg.conversations.ask.effective_backend(&cfg.llm),
        cfg.conversations.ask.effective_rewriter_backend(&cfg.llm),
        cfg.conversations.rollup.effective_extractive_backend(&cfg.llm),
        cfg.conversations.rollup.effective_abstractive_backend(&cfg.llm),
    ];
```

`mur-core/src/cmd/conversations_cmd.rs:1242` — the `--model` flag default came from `ask_cfg.model`; take it from the resolved backend instead:

```rust
    let effective = ask_cfg.effective_backend(&cfg.llm);
    let model = args.model.clone().unwrap_or_else(|| effective.model.clone());
```

`mur-core/src/cmd/conversations_cmd.rs:1302` — delete the `endpoint: ask_cfg.ollama_endpoint.clone(),` field from the struct literal and take the endpoint from `effective.endpoint` instead. If the surrounding struct wants a bare `String`, use `effective.endpoint.clone().unwrap_or_default()`.

`mur-core/src/cmd/skill_evolve.rs:15`:

```rust
    let backend = cfg.conversations.ask.effective_backend(&cfg.llm);
```

- [ ] **Step 5: Build and run the full test suites**

```bash
export ORT_STRATEGY=download
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo nextest run -p mur-common
RUST_MIN_STACK=33554432 cargo nextest run -p mur-core conversations
cargo clippy -p mur-common -p mur-core -- -D warnings
```

Expected: green. Every test passes; nothing is left knowingly red.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(config): delete the legacy conversation model fields"
```

---

### Task 5: `embedding.ollama_endpoint` becomes optional

With `provider: omlx` this field is read only in the `_ =>` fallback arm of `EmbeddingConfig::from_config` (`mur-core/src/store/embedding.rs:107-125`) — it is dead, yet reserialization keeps writing it back so deleting it by hand never sticks.

**Files:**
- Modify: `mur-common/src/config.rs:418-420` (`EmbeddingConfig.ollama_endpoint`) and its `Default` impl
- Modify: `mur-core/src/store/embedding.rs:122-124`
- Modify: `mur-core/src/cmd/misc.rs:296`

**Interfaces:**
- Consumes: nothing new.
- Produces: `EmbeddingConfig.ollama_endpoint: Option<String>`.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `mur-common/src/config.rs`:

```rust
    #[test]
    fn embedding_ollama_endpoint_is_omitted_when_unset() {
        let mut cfg = Config::default();
        cfg.embedding.provider = "omlx".into();
        cfg.embedding.openai_url = Some("http://127.0.0.1:8000/v1".into());
        cfg.embedding.ollama_endpoint = None;
        let yaml = serde_yaml_ng::to_string(&cfg).expect("serializes");
        assert!(!yaml.contains("ollama_endpoint"), "dead field re-emitted:\n{yaml}");
    }

    #[test]
    fn embedding_ollama_endpoint_still_round_trips_when_set() {
        let yaml = "provider: ollama\nmodel: nomic-embed-text\nollama_endpoint: http://box.local:11434\n";
        let e: EmbeddingConfig = serde_yaml_ng::from_str(yaml).expect("parses");
        assert_eq!(e.ollama_endpoint.as_deref(), Some("http://box.local:11434"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run -p mur-common embedding_ollama_endpoint
```

Expected: FAIL — `expected String, found Option<String>`.

- [ ] **Step 3: Change the field**

In `mur-common/src/config.rs`, replace:

```rust
    /// Ollama endpoint
    #[serde(default = "default_ollama_endpoint")]
    pub ollama_endpoint: String,
```

with:

```rust
    /// Ollama endpoint. `None` for every non-Ollama provider — the OpenAI
    /// path uses `openai_url`. Kept out of the serialized document when
    /// unset so it stops reappearing in configs that never use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_endpoint: Option<String>,
```

In `impl Default for EmbeddingConfig`, change `ollama_endpoint: default_ollama_endpoint(),` to `ollama_endpoint: Some(default_ollama_endpoint()),`.

In `mur-core/src/store/embedding.rs`, the `_ =>` arm:

```rust
            _ => EmbeddingProvider::Ollama {
                base_url: cfg
                    .embedding
                    .ollama_endpoint
                    .clone()
                    .unwrap_or_else(|| "http://localhost:11434".into()),
            },
```

In `mur-core/src/cmd/misc.rs:296`:

```rust
        "ollama" => emb.ollama_endpoint.clone().unwrap_or_default(),
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
export ORT_STRATEGY=download
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo nextest run -p mur-common
cargo nextest run -p mur-core embedding
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "fix(config): stop re-emitting the dead embedding.ollama_endpoint"
```

---

### Task 6: Wire the migration into both load paths

**Files:**
- Modify: `mur-common/src/config.rs:353-358` (`Config::load_or_default`)
- Modify: `mur-core/src/store/config.rs:9-24` (`load_config`)
- Test: `mur-core/tests/config_migration.rs` (create)

**Interfaces:**
- Consumes: `mur_common::config_migrate::migrate_conversations_yaml` from Task 3.
- Produces: no new API. Behavior: `load_config()` migrates **and writes back**; `load_or_default()` migrates **in memory only**.

Why the asymmetry: `load_or_default` is called from `mur-agent-runtime` (`tools/fleet_run.rs:56,124`, `supervisor_runner.rs:363,595`). Agent runtime processes must never write the user's config. They still need the in-memory conversion, otherwise a pinned stage would silently start inheriting the smart slot.

- [ ] **Step 1: Write the failing test**

Create `mur-core/tests/config_migration.rs`:

```rust
//! Loading a config with legacy conversation fields migrates it on disk, once.

use std::fs;

// NOTE: override MUR_HOME, never HOME. `store::config::config_path()` checks
// MUR_HOME first precisely because `dirs::home_dir()` ignores HOME on Windows
// — a test that overrode only HOME would migrate the developer's real
// ~/.mur/config.yaml on a Windows CI runner.
#[test]
fn load_config_migrates_legacy_fields_and_writes_back() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("config.yaml");
    fs::write(
        &cfg_path,
        "conversations:\n  ask:\n    model: llama3:70b\n    ollama_endpoint: http://box.local:11434\n",
    )
    .unwrap();

    let prev = std::env::var("MUR_HOME").ok();
    unsafe { std::env::set_var("MUR_HOME", tmp.path()) };

    let cfg = mur_core::store::config::load_config().expect("loads");
    let b = cfg.conversations.ask.backend.clone().expect("pinned by migration");
    assert_eq!(b.provider, "ollama");
    assert_eq!(b.model, "llama3:70b");
    assert_eq!(b.endpoint.as_deref(), Some("http://box.local:11434"));

    let on_disk = fs::read_to_string(&cfg_path).unwrap();
    assert!(on_disk.contains("backend:"), "migration written back:\n{on_disk}");
    assert!(!on_disk.contains("ollama_endpoint: http://box.local"), "legacy key removed");

    // Second load is a no-op: byte-identical file.
    let before = on_disk.clone();
    let _ = mur_core::store::config::load_config().expect("loads again");
    assert_eq!(fs::read_to_string(&cfg_path).unwrap(), before, "idempotent");

    unsafe {
        match prev {
            Some(h) => std::env::set_var("MUR_HOME", h),
            None => std::env::remove_var("MUR_HOME"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
export ORT_STRATEGY=download
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo nextest run -p mur-core config_migration
```

Expected: FAIL — `backend` is `None`, nothing was written.

- [ ] **Step 3: Wire it into both loaders**

In `mur-common/src/config.rs`, replace `load_or_default`:

```rust
    /// Load a config, falling back to defaults. Legacy conversation fields
    /// are migrated **in memory only** — this is called from agent runtime
    /// processes, which must never write the user's config file.
    pub fn load_or_default(path: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let text = crate::config_migrate::migrate_conversations_yaml(&text).unwrap_or(text);
        serde_yaml_ng::from_str(&text).unwrap_or_default()
    }
```

In `mur-core/src/store/config.rs`, inside `load_config` after the `read_to_string`:

```rust
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config: {}", path.display()))?;

    // One-shot migration of the legacy conversations model fields. Runs here,
    // at the CLI's own load, because every `save_config` caller loads first —
    // migrating on read is what closes the window in which a save would erase
    // the legacy keys before they could be converted.
    let content = match mur_common::config_migrate::migrate_conversations_yaml(&content) {
        Some(migrated) => {
            // Atomic write: temp file beside the target, then rename — the
            // same inline pattern `save_config_at` uses. `rename` is only
            // atomic within a filesystem, so the temp file must live next to
            // `path`, not in a system temp directory.
            let tmp_path = path.with_extension("yaml.tmp");
            fs::write(&tmp_path, &migrated)
                .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
            fs::rename(&tmp_path, &path)
                .with_context(|| format!("Failed to rename temp to final: {}", path.display()))?;
            println!(
                "MUR: migrated conversations model settings in {} to explicit backends",
                path.display()
            );
            migrated
        }
        None => content,
    };

    let config: Config = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse config: {}", path.display()))?;
    Ok(config)
```

Note this writes the migrated YAML *without* the comment header that `save_config_at` prepends. That is deliberate: the header is regenerated on the next `save_config`, and reproducing it here would duplicate a 40-line string literal. If a test asserts the header survives a migration, prefer routing the write through `save_config_at` instead — but only after confirming it does not re-introduce the legacy keys.

- [ ] **Step 4: Run tests to verify they pass**

```bash
export ORT_STRATEGY=download
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo nextest run -p mur-core config_migration
cargo nextest run -p mur-common
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(config): migrate legacy conversation fields on load"
```

---

### Task 7: Writers stop emitting bare model names

**Files:**
- Modify: `mur-core/src/model_setup/mod.rs:43-48,206,244-248,386-388`
- Modify: `mur-core/src/model_setup/slots.rs:64-90,229-272`

**Interfaces:**
- Consumes: the six `effective_backend` resolvers (Task 2), `local_slot_choice` (`model_setup/mod.rs:141-156`, already present).
- Produces: `ModelSetupPlan.conversations: Option<SlotChoice>` (renamed from `conversations_model: Option<String>`).

- [ ] **Step 1: Write the failing test**

Replace the existing assertions at `mur-core/src/model_setup/mod.rs:386-388` with:

```rust
        let b = cfg
            .conversations
            .ask
            .backend
            .as_ref()
            .expect("setup writes an explicit backend, not a bare model name");
        assert_eq!(b.provider, "ollama");
        assert_eq!(b.model, "qwen3.5:4b");
        assert_eq!(
            cfg.conversations.compact.extractive_backend.as_ref().map(|b| b.model.as_str()),
            Some("qwen3.5:4b")
        );
        assert_eq!(
            cfg.conversations.rollup.extractive_backend.as_ref().map(|b| b.model.as_str()),
            Some("qwen3.5:4b")
        );
```

And append a test proving the omlx case — the exact bug this whole change exists to fix:

```rust
    #[test]
    fn an_omlx_local_model_reaches_the_conversation_stages_as_openai() {
        use crate::discovery::{Backend, DiscoveredModel, ModelKind};

        let discovered = vec![DiscoveredModel {
            id: "Qwen3.5-4B-MLX-4bit".into(),
            backend: Backend::OMlx,
            kind: ModelKind::Llm,
            dims: None,
            family: None,
            size_bytes: None,
            probed_at: None,
        }];
        let plan = recommend(&discovered, &[]);
        let mut cfg = Config::default();
        apply(&plan, &mut cfg);
        let b = cfg.conversations.ask.backend.expect("backend written");
        assert_eq!(b.provider, "openai", "omlx must not be left as an Ollama name");
        assert_eq!(b.endpoint.as_deref(), Some("http://localhost:8000/v1"));
        assert_eq!(b.api_key_ref.as_deref(), Some("env:OMLX_API_KEY"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
export ORT_STRATEGY=download
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo nextest run -p mur-core model_setup
```

Expected: FAIL — `no field 'backend'` / the plan still carries a bare string.

- [ ] **Step 3: Rewrite the plan field and `apply`**

In `mur-core/src/model_setup/mod.rs`, change the struct field at `:46`:

```rust
    /// Backend for the three conversation stages. Deliberately derived from
    /// the *local* model rather than `smart`: conversation stages stay
    /// on-device even when a cloud key is present.
    pub conversations: Option<SlotChoice>,
```

At `:206`, keep the local preference but stop discarding the backend:

```rust
    let conversations = local_llm.as_ref().map(local_slot_choice);
```

Update the `ModelSetupPlan { .. }` literal at `:222-227` to use `conversations`.

Replace `apply`'s third block (`:244-248`):

```rust
    if let Some(c) = &plan.conversations {
        let backend = BackendConfig {
            provider: c.provider.clone(),
            model: c.model.clone(),
            endpoint: c.openai_url.clone(),
            api_key_env: None,
            api_key_ref: c.api_key_ref.clone(),
            timeout_secs: None,
        };
        config.conversations.ask.backend = Some(backend.clone());
        config.conversations.compact.extractive_backend = Some(backend.clone());
        config.conversations.rollup.extractive_backend = Some(backend);
    }
```

Add `use mur_common::config::BackendConfig;` to the file's imports if absent.

- [ ] **Step 4: Update the Hub slot setter**

In `mur-core/src/model_setup/slots.rs`, the three `*_pair` readers (`:64-90`) currently hardcode `"ollama"` for the `None` case. Replace each with the resolver, e.g.:

```rust
fn ask_pair(cfg: &Config) -> (String, String) {
    let b = cfg.conversations.ask.effective_backend(&cfg.llm);
    (b.provider, b.model)
}

fn compact_pair(cfg: &Config) -> (String, String) {
    let b = cfg.conversations.compact.effective_extractive_backend(&cfg.llm);
    (b.provider, b.model)
}

fn rollup_pair(cfg: &Config) -> (String, String) {
    let b = cfg.conversations.rollup.effective_extractive_backend(&cfg.llm);
    (b.provider, b.model)
}
```

In `write_conversation_stage` (`:229-272`), both `Local` and `Registry` now write a real backend, and the rollup `bail!` goes away:

```rust
fn write_conversation_stage(
    cfg: &mut Config,
    id: SlotId,
    _sel: &SlotSelection,
    r: &Resolved,
) -> Result<()> {
    let backend = Some(BackendConfig {
        provider: r.provider.clone(),
        model: r.model.clone(),
        endpoint: r.endpoint.clone(),
        api_key_env: None,
        api_key_ref: r.api_key_ref.clone(),
        timeout_secs: None,
    });
    match id {
        SlotId::Ask => cfg.conversations.ask.backend = backend,
        SlotId::Compact => cfg.conversations.compact.extractive_backend = backend,
        SlotId::Rollup => cfg.conversations.rollup.extractive_backend = backend,
        _ => unreachable!("write_conversation_stage only called for Ask/Compact/Rollup"),
    }
    Ok(())
}
```

The `sel` parameter is now unused; either drop it from the signature and its call site, or keep it prefixed with `_` as shown. Drop the now-unused `bail` import if clippy flags it.

- [ ] **Step 5: Run tests and clippy**

```bash
export ORT_STRATEGY=download
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
RUST_MIN_STACK=33554432 cargo nextest run -p mur-core model_setup
cargo clippy -p mur-core -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix(model-setup): write real backends into the conversation stages"
```

---

### Task 8: doctor reports what each stage actually dials

**Files:**
- Create: `mur-core/src/cmd/conversations_doctor_backends.rs`
- Modify: `mur-core/src/cmd/mod.rs` (register the module)
- Modify: `mur-core/src/cmd/conversations_cmd.rs:508-560,770-800` (delete both unconditional Ollama probes and the anthropic-only filter; call the new module)
- Test: `mur-core/tests/doctor_backends.rs` (create)

New module rather than growing `conversations_cmd.rs`, which is already 1592 lines against an 800-line limit.

**Interfaces:**
- Consumes: the six `effective_backend` resolvers (Task 2).
- Produces:
  - `pub struct StageBackend { pub stage: &'static str, pub backend: BackendConfig, pub pinned: bool }`
  - `pub fn stage_backends(cfg: &Config) -> Vec<StageBackend>` — the six stages in listing order
  - `pub async fn probe(backends: &[StageBackend]) -> Vec<ProbeResult>` — one probe per unique `(provider, model, endpoint)`
  - `pub struct ProbeResult { pub endpoint: String, pub ok: bool, pub detail: String, pub used_by: Vec<&'static str> }`

- [ ] **Step 1: Write the failing test**

Create `mur-core/tests/doctor_backends.rs`:

```rust
//! doctor names the endpoint each conversation stage will dial, and probes it.

use mur_common::config::{BackendConfig, Config};

#[test]
fn lists_all_six_stages_and_marks_pinned_vs_inherited() {
    let mut cfg = Config::default();
    cfg.llm.provider = "omlx".into();
    cfg.llm.model = "Qwen3.5-4B-MLX-4bit".into();
    cfg.llm.openai_url = Some("http://127.0.0.1:8000/v1".into());
    cfg.conversations.compact.abstractive_backend = Some(BackendConfig {
        provider: "ollama".into(),
        model: "qwen3:4b".into(),
        endpoint: Some("http://localhost:11434".into()),
        api_key_env: None,
        api_key_ref: None,
        timeout_secs: None,
    });

    let rows = mur_core::cmd::conversations_doctor_backends::stage_backends(&cfg);
    assert_eq!(rows.len(), 6, "six call sites");

    let ask = rows.iter().find(|r| r.stage == "ask.generate").unwrap();
    assert!(!ask.pinned, "inherits the smart slot");
    assert_eq!(ask.backend.provider, "openai", "omlx maps to openai");
    assert_eq!(ask.backend.endpoint.as_deref(), Some("http://127.0.0.1:8000/v1"));

    let abs = rows.iter().find(|r| r.stage == "compact.abstractive").unwrap();
    assert!(abs.pinned);
    assert_eq!(abs.backend.endpoint.as_deref(), Some("http://localhost:11434"));
}

#[tokio::test]
async fn probes_each_unique_endpoint_once_and_reports_who_uses_it() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"object":"list","data":[{"id":"Qwen3.5-4B-MLX-4bit"}]}"#,
        ))
        .mount(&server)
        .await;

    let mut cfg = Config::default();
    cfg.llm.provider = "openai".into();
    cfg.llm.model = "Qwen3.5-4B-MLX-4bit".into();
    cfg.llm.openai_url = Some(server.uri());
    cfg.llm.api_key_ref = Some("env:MUR_TEST_DOCTOR_KEY".into());
    unsafe { std::env::set_var("MUR_TEST_DOCTOR_KEY", "k") };

    let rows = mur_core::cmd::conversations_doctor_backends::stage_backends(&cfg);
    let results = mur_core::cmd::conversations_doctor_backends::probe(&rows).await;

    assert_eq!(results.len(), 1, "six stages, one endpoint, one probe");
    assert!(results[0].ok, "reachable: {}", results[0].detail);
    assert!(results[0].detail.contains("Qwen3.5-4B-MLX-4bit"), "model presence reported");
    assert_eq!(results[0].used_by.len(), 6);

    unsafe { std::env::remove_var("MUR_TEST_DOCTOR_KEY") };
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
export ORT_STRATEGY=download
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo nextest run -p mur-core doctor_backends
```

Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement the module**

Create `mur-core/src/cmd/conversations_doctor_backends.rs`:

```rust
//! Per-stage backend listing and endpoint probing for `mur chat doctor`.
//!
//! Replaces the previous unconditional Ollama probe, which read
//! `compact.ollama_endpoint` whether or not any stage still used Ollama, and
//! the cloud probe that only ever looked at `provider == "anthropic"`.

use std::collections::BTreeMap;
use std::time::Duration;

use mur_common::config::{BackendConfig, Config};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub struct StageBackend {
    pub stage: &'static str,
    pub backend: BackendConfig,
    /// True when the user pinned this stage; false when it inherits `llm`.
    pub pinned: bool,
}

pub struct ProbeResult {
    pub endpoint: String,
    pub ok: bool,
    pub detail: String,
    pub used_by: Vec<&'static str>,
}

/// The six chat call sites, in listing order.
pub fn stage_backends(cfg: &Config) -> Vec<StageBackend> {
    let a = &cfg.conversations.ask;
    let c = &cfg.conversations.compact;
    let r = &cfg.conversations.rollup;
    vec![
        StageBackend {
            stage: "ask.generate",
            backend: a.effective_backend(&cfg.llm),
            pinned: a.backend.is_some(),
        },
        StageBackend {
            stage: "ask.rewriter",
            backend: a.effective_rewriter_backend(&cfg.llm),
            pinned: a.rewriter_backend.is_some(),
        },
        StageBackend {
            stage: "compact.extractive",
            backend: c.effective_extractive_backend(&cfg.llm),
            pinned: c.extractive_backend.is_some(),
        },
        StageBackend {
            stage: "compact.abstractive",
            backend: c.effective_abstractive_backend(&cfg.llm),
            pinned: c.abstractive_backend.is_some(),
        },
        StageBackend {
            stage: "rollup.extractive",
            backend: r.effective_extractive_backend(&cfg.llm),
            pinned: r.extractive_backend.is_some(),
        },
        StageBackend {
            stage: "rollup.abstractive",
            backend: r.effective_abstractive_backend(&cfg.llm),
            pinned: r.abstractive_backend.is_some(),
        },
    ]
}

fn endpoint_of(b: &BackendConfig) -> String {
    b.endpoint.clone().unwrap_or_else(|| match b.provider.as_str() {
        "ollama" => "http://localhost:11434".into(),
        "anthropic" => "https://api.anthropic.com".into(),
        "openai" => "https://api.openai.com/v1".into(),
        "openrouter" => "https://openrouter.ai/api/v1".into(),
        "gemini" => "https://generativelanguage.googleapis.com".into(),
        _ => "(unset)".into(),
    })
}

/// One probe per unique (provider, model, endpoint). Never prints or returns
/// a secret value — only whether the key resolved.
pub async fn probe(stages: &[StageBackend]) -> Vec<ProbeResult> {
    let mut groups: BTreeMap<(String, String, String), Vec<&'static str>> = BTreeMap::new();
    for s in stages {
        let key = (
            s.backend.provider.clone(),
            s.backend.model.clone(),
            endpoint_of(&s.backend),
        );
        groups.entry(key).or_default().push(s.stage);
    }

    let mut out = Vec::new();
    for ((provider, model, endpoint), used_by) in groups {
        let (ok, detail) = match provider.as_str() {
            "ollama" => probe_list(&format!("{}/api/tags", endpoint.trim_end_matches('/')), &model).await,
            "openai" | "openrouter" => {
                probe_list(&format!("{}/models", endpoint.trim_end_matches('/')), &model).await
            }
            // No cheap list endpoint, and a real call would cost money.
            // Report only whether the credential resolves.
            _ => {
                let resolvable = stages
                    .iter()
                    .find(|s| s.backend.provider == provider)
                    .map(|s| key_resolves(&s.backend))
                    .unwrap_or(false);
                if resolvable {
                    (true, "API key resolves (not probed)".into())
                } else {
                    (false, "API key does not resolve".into())
                }
            }
        };
        out.push(ProbeResult { endpoint, ok, detail, used_by });
    }
    out
}

async fn probe_list(url: &str, model: &str) -> (bool, String) {
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return (false, format!("client build failed: {e}")),
    };
    match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            let present = body.contains(model);
            let n = body.matches("\"id\"").count().max(body.matches("\"name\"").count());
            if present {
                (true, format!("{n} models, {model} present"))
            } else {
                (false, format!("{n} models, {model} NOT present"))
            }
        }
        Ok(resp) => (false, format!("returned {}", resp.status())),
        Err(e) => (false, format!("unreachable: {e}")),
    }
}

fn key_resolves(b: &BackendConfig) -> bool {
    if let Some(r) = b.api_key_ref.as_deref() {
        return r
            .parse::<mur_common::secret::SecretRef>()
            .ok()
            .and_then(|s| s.resolve_to_string_blocking())
            .is_some();
    }
    let var = b.api_key_env.as_deref().unwrap_or(match b.provider.as_str() {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        _ => "LLM_API_KEY",
    });
    std::env::var(var).is_ok()
}

/// Print the table and probe results. Returns false when an endpoint that a
/// stage actually uses is unreachable — an idle Ollama nobody references must
/// not turn doctor red.
pub async fn report(cfg: &Config) -> bool {
    let stages = stage_backends(cfg);
    println!("conversations backends");
    for s in &stages {
        println!(
            "  {:<20} {:<10} {:<24} {:<26} [{}]",
            s.stage,
            s.backend.provider,
            s.backend.model,
            endpoint_of(&s.backend),
            if s.pinned { "pinned" } else { "follows smart" }
        );
    }
    let results = probe(&stages).await;
    let mut ok = true;
    for r in &results {
        if r.ok {
            println!("  ✓ {} — {}", r.endpoint, r.detail);
        } else {
            println!(
                "  ✗ {} — {} (used by: {})",
                r.endpoint,
                r.detail,
                r.used_by.join(", ")
            );
            ok = false;
        }
    }
    ok
}
```

Register it in `mur-core/src/cmd/mod.rs`:

```rust
pub mod conversations_doctor_backends;
```

- [ ] **Step 4: Delete the two old probes and call the new one**

In `mur-core/src/cmd/conversations_cmd.rs`:

- Delete the Ollama probe block at `:508-526` (from `let endpoint = cfg.conversations.compact.ollama_endpoint.clone();` through the `else { println!("  · Ollama not reachable …") }`).
- Delete the cloud-probe block at `:528-560`, including the `provider == "anthropic"` filter and the now-unused `collect_backend_configs`.
- Delete the duplicate probe block at `:770-800`.
- At the point where the first block stood, insert:

```rust
    if !crate::cmd::conversations_doctor_backends::report(&cfg).await {
        ok = false;
    }
```

- [ ] **Step 5: Run tests and clippy**

```bash
export ORT_STRATEGY=download
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo nextest run -p mur-core doctor_backends
RUST_MIN_STACK=33554432 cargo nextest run -p mur-core conversations
cargo clippy -p mur-core -- -D warnings
cargo fmt --all
```

Expected: PASS.

- [ ] **Step 6: Verify against the real machine**

```bash
cargo run -p mur-core --bin mur -- chat doctor
```

Expected: a six-row table. Any stage inheriting from an `omlx` smart slot must show `provider: openai` and the `:8000/v1` endpoint — never `ollama` / `:11434`.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(doctor): report and probe the backend each conversation stage dials"
```

---

### Task 9: Full-workspace verification and docs

**Files:**
- Modify: `README.md` — the `conversations` config example, if it shows the legacy fields
- Modify: `CLAUDE.md` — only if a documented CLI surface changed (it does not; no command was added or removed)

- [ ] **Step 1: Grep for surviving references to the deleted fields**

```bash
rg -n 'ollama_endpoint|extractive_model|abstractive_model|synthesize_.*backend' \
  --glob '!target' --glob '!*.lock' .
```

Expected: hits only in `mur-common/src/config_migrate.rs` (which must still know the legacy names), `EmbeddingConfig`, and `docs/superpowers/specs/`. Any hit in `mur-core/src` or `README.md` is a miss — fix it.

- [ ] **Step 2: Full workspace build and test**

```bash
export ORT_STRATEGY=download
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo nextest run --workspace
cargo test --workspace --no-run
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
```

`cargo test --workspace --no-run` matters even though nextest is the runner: it is what catches struct-literal breakage in workspace-excluded crates' fixtures.

- [ ] **Step 3: Check the excluded Tauri crates still compile**

`mur-hub-gui` consumes `mur_core::model_setup::slots` (`mur-hub-gui/src-tauri/src/model_slots.rs:13`), which Task 7 changed.

```bash
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib -- -D warnings
```

Expected: PASS. CI runs clippy as `--lib` on a fresh compile, so a local full build passing is not sufficient evidence.

- [ ] **Step 4: Update README if needed**

```bash
rg -n 'ollama_endpoint|extractive_model' README.md
```

If any hit shows a `conversations:` example, replace the legacy keys with a backend override block:

```yaml
conversations:
  ask:
    backend:
      provider: openai
      model: Qwen3.5-4B-MLX-4bit
      endpoint: http://127.0.0.1:8000/v1
      api_key_ref: env:OMLX_API_KEY
```

and note that omitting `backend:` makes the stage follow the `llm:` block.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: conversation stages configure a backend, not a bare model name"
```

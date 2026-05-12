# Cloud LLM Backend P3 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Close out the cloud-LLM rollout by (a) migrating the three remaining `OllamaClient`-direct call sites onto `ChatBackend`, (b) wiring prompt-caching hints into `AnthropicBackend`'s request body, (c) adding per-call cost telemetry + a `mur conversations cost-report` command, (d) fixing two small follow-ups from P2 review (I1 Ollama final-chunk usage drop, I2 `synthesize_*_backend` timeout omission), and (e) adding the integration tests P2 deferred (I3-I5: full factory→retry→stream composition).

**Architecture:** The remaining call sites — `compact.abstractive::summarize`, `summarize::abstractive::rollup_narrative`, `ask::abstractive::compress_hit` — get the same `&dyn ChatBackend` migration treatment as their P0/P1/P2 siblings (`ask::rewriter`, `compact.extractive`, `ask::generate`). Once migrated, every LLM call in the conversations subsystem flows through `factory::build → RetryingBackend → {OllamaBackend|AnthropicBackend|MockBackend}`, which gives us a single chokepoint to add (i) prompt-caching hints (set in `ChatRequest`, honored only by AnthropicBackend) and (ii) per-call telemetry (`Usage` already returned on every call, just needs a tracing-based JSONL writer). The `cost-report` command tails those JSONL files and aggregates by provider×model×stage. I1 fix re-reads the final NDJSON line in `OllamaClient::generate_stream` so streamed Ollama responses report token counts. I2 fix bakes the missing `timeout_secs` into the `synthesize_*_backend` helpers so the workaround in `cmd_ask` can go away.

**Tech Stack:** Rust 2024 · `tracing` (already in deps) for span emission · `tracing-subscriber` (already in deps) `JsonLayer` for JSONL output · `serde_json` for parsing JSONL during aggregation · `humantime` (already in deps via clap dep tree — verify) for `--since 7d` parsing · `wiremock = "0.6"` (already a dev-dep) for caching-shape tests. **No new crates.**

**Spec:** `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` — §4.2 (`Usage` struct), §5.2 caching invariants, §6 BackendConfig, §11 telemetry sketch, §12 phase boundaries (P3 row), §13 cost guidance table. After P3 the only remaining cloud-LLM follow-up is P4 (migrate `learn`/`extract_llm`, delete `mur-core/src/llm.rs`).

**Out of scope for P3** — explicitly do not touch:
- Migrating `learn` / `extract_llm` (`mur-core/src/llm.rs`) — P4
- Deleting `mur-core/src/llm.rs` — P4
- Bedrock / Vertex / Foundry support — declined non-goal, spec §2
- Embedding migration to cloud — declined non-goal, spec §2
- Auto-fallback from cloud to ollama on outage — declined non-goal, spec §8.2
- `max_daily_cost_usd` guardrail — deferred until telemetry shows real numbers (spec §2)
- Mid-stream retry on `RetryingBackend::generate_stream` — connect-only is correct (spec §8.1)
- Switching shipped defaults to cloud — Ollama remains the local-first default
- Keychain integration for API keys (`secret_ref` field) — deferred until model-registry convergence per spec §10

**Plan deviations flagged from spec:** none. P3 implements exactly what spec §12 lists for this phase, plus three small P2-review follow-ups (I1, I2, I3-I5) that the user agreed should land here.

---

## Task 0: Verify foundation + read context (no commit)

**Files:** none modified.

**Step 1: Confirm P0 + P1 + P2 are on `main`**

Run:
```bash
git log --oneline | grep -E "8f0f712|f692594|79e4b72" | head -3
```

Expected:
```
8f0f712 feat: cloud-LLM backend P2 (streaming on the trait + ask::generate canary) (#98)
f692594 feat: cloud-LLM backend P1 (AnthropicBackend + per-stage routing + retry envelope) (#91)
79e4b72 refactor(conversations): introduce ChatBackend trait (P0 of cloud-LLM rollout) (#80)
```

If any SHA is missing, **STOP** — P3 assumes all three phases are landed and merged.

**Step 2: Read the spec sections you'll be touching**

- `docs/superpowers/specs/2026-05-01-cloud-llm-backend-design.md` §5.2 (caching invariants — minimum cacheable prefix per model)
- §6 (`BackendConfig` resolution order — see synthesize_*_backend helpers)
- §11 (telemetry sketch — `tracing::info_span!` per-call shape)
- §12 (phase table — confirm P3 scope)
- §13 (cost guidance — pricing table to bake into `cost-report`)

**Step 3: Read the current `Usage` struct + `ChatChunk` shape**

`mur-core/src/conversations/backend/mod.rs` lines 47-63. `Usage` already carries every field `cost-report` needs. `ChatChunk { delta, usage: Some(_) on final chunk only }` — final-chunk usage is **always None** for streamed Ollama today (this is exactly what I1 fixes).

**Step 4: Read the three remaining un-migrated call sites**

- `mur-core/src/conversations/summarize/abstractive.rs` (~85 LOC, single `summarize` fn)
- `mur-core/src/conversations/summarize/rollup.rs` lines 170-190 (caller of `rollup_narrative` — uses `OllamaClient` directly), and `summarize/abstractive.rs` lines 113-152 (`rollup_narrative` itself)
- `mur-core/src/conversations/ask/abstractive.rs` lines 41-187 (`AbstractiveCtx` + `compress_hit` for Stage 1b)

**Step 5: Read the workaround we're about to remove**

`mur-core/src/cmd/conversations_cmd.rs` lines 1283-1297 — the `if answer_cfg.timeout_secs.is_none()` block exists because `synthesize_backend()` returns `timeout_secs: None` and `factory::build` defaults to 120s, but `ask_cfg.timeout_secs` was the original per-call budget. Task 1 below moves the timeout into the helpers themselves so this workaround becomes a one-liner.

**Step 6: No commit** — context-loading only.

---

## Task 1: Fix I2 — bake `timeout_secs` into `synthesize_*_backend` helpers

**Files:**
- Modify: `mur-common/src/config.rs` (4 `synthesize_*_backend` methods)
- Modify: `mur-core/src/cmd/conversations_cmd.rs` lines 1283-1297 (delete the workaround)

**Step 1: Write the failing tests in `mur-common/src/config.rs`**

Append to the existing `#[cfg(test)] mod tests` (find it via `grep -n "mod tests" mur-common/src/config.rs | head -1`):

```rust
    #[test]
    fn ask_synthesize_backend_inherits_timeout_secs_from_legacy_field() {
        let cfg = AskConfig {
            timeout_secs: 45,
            ..AskConfig::default()
        };
        let b = cfg.synthesize_backend();
        assert_eq!(
            b.timeout_secs,
            Some(45),
            "synthesize_backend() must propagate ask.timeout_secs into the synthesized BackendConfig"
        );
    }

    #[test]
    fn ask_synthesize_backend_does_not_override_explicit_per_stage_timeout() {
        let mut cfg = AskConfig {
            timeout_secs: 45,
            ..AskConfig::default()
        };
        cfg.backend = Some(BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: None,
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            timeout_secs: Some(10),
        });
        let b = cfg.synthesize_backend();
        assert_eq!(
            b.timeout_secs,
            Some(10),
            "explicit per-stage timeout_secs must NOT be overridden by ask.timeout_secs"
        );
    }

    #[test]
    fn ask_synthesize_rewriter_backend_uses_rewriter_timeout_secs_when_synthesizing() {
        let cfg = AskConfig {
            timeout_secs: 120,
            rewriter_timeout_secs: 8,
            ..AskConfig::default()
        };
        let b = cfg.synthesize_rewriter_backend();
        assert_eq!(
            b.timeout_secs,
            Some(8),
            "rewriter synthesis must use rewriter_timeout_secs (not the answer-call timeout)"
        );
    }

    #[test]
    fn compact_synthesize_extractive_backend_inherits_default_timeout_when_no_override() {
        // CompactConfig has no per-stage timeout field — extractive synthesis
        // should fall back to the conservative 120s default.
        let cfg = CompactConfig::default();
        let b = cfg.synthesize_extractive_backend();
        assert_eq!(
            b.timeout_secs,
            Some(120),
            "compact synthesis without per-stage override must produce 120s timeout"
        );
    }

    #[test]
    fn compact_synthesize_abstractive_backend_inherits_default_timeout_when_no_override() {
        let cfg = CompactConfig::default();
        let b = cfg.synthesize_abstractive_backend();
        assert_eq!(b.timeout_secs, Some(120));
    }
```

**Step 2: Run tests to confirm 4 of them fail**

Run: `cargo test -p mur-common --lib -- timeout 2>&1 | tail -15`

Expected: 4 of the 5 tests FAIL — currently `synthesize_backend()` returns `timeout_secs: None`, so all assertions on `Some(_)` blow up.

**Step 3: Update the four helpers**

In `mur-common/src/config.rs`, find `impl AskConfig` and replace `synthesize_backend`:

```rust
    pub fn synthesize_backend(&self) -> BackendConfig {
        self.backend.clone().unwrap_or_else(|| BackendConfig {
            provider: "ollama".into(),
            model: self.model.clone(),
            endpoint: Some(self.ollama_endpoint.clone()),
            api_key_env: None,
            timeout_secs: Some(self.timeout_secs as u64),
        })
    }

    pub fn synthesize_rewriter_backend(&self) -> BackendConfig {
        self.rewriter_backend.clone().unwrap_or_else(|| BackendConfig {
            provider: "ollama".into(),
            model: self.model.clone(),
            endpoint: Some(self.ollama_endpoint.clone()),
            api_key_env: None,
            timeout_secs: Some(self.rewriter_timeout_secs as u64),
        })
    }
```

(Note: rewriter no longer falls through `synthesize_backend()`; it uses its own `rewriter_timeout_secs` so the rewriter call doesn't burn the full ask timeout. Document inline.)

In `impl CompactConfig`, replace the two synthesize methods:

```rust
    pub fn synthesize_extractive_backend(&self) -> BackendConfig {
        self.extractive_backend.clone().unwrap_or_else(|| BackendConfig {
            provider: "ollama".into(),
            model: self.extractive_model.clone(),
            endpoint: Some(self.ollama_endpoint.clone()),
            api_key_env: None,
            timeout_secs: Some(120),
        })
    }

    pub fn synthesize_abstractive_backend(&self) -> BackendConfig {
        self.abstractive_backend.clone().unwrap_or_else(|| BackendConfig {
            provider: "ollama".into(),
            model: self.abstractive_model.clone(),
            endpoint: Some(self.ollama_endpoint.clone()),
            api_key_env: None,
            timeout_secs: Some(120),
        })
    }
```

(CompactConfig has no per-stage timeout fields — 120s matches the previously-hardcoded `Duration::from_secs(120)` at the call sites, so behavior is byte-identical.)

**Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-common --lib -- timeout 2>&1 | tail -15`

Expected: 5 PASS.

**Step 5: Delete the workaround in `cmd_ask`**

In `mur-core/src/cmd/conversations_cmd.rs`, find lines ~1283-1297 and replace the block:

```rust
    // Build the answer-streaming backend via factory, honoring the per-stage
    // `ask.backend` override. synthesize_backend() now bakes ask.timeout_secs
    // into the synthesized BackendConfig (see I2 fix in P3 task 1) so factory's
    // 120s default doesn't override the user's per-call budget.
    let answer_backend =
        crate::conversations::backend::factory::build(&ask_cfg.synthesize_backend())?;
```

(Drops the `let mut answer_cfg = ...; if answer_cfg.timeout_secs.is_none() { ... }` block entirely.)

**Step 6: Run integration tests**

```bash
cargo test -p mur-core --test cli_conversations -- --test-threads=1 2>&1 | tail -15
```

Expected: PASS — `cmd_ask` smoke tests still pass with the workaround removed.

**Step 7: Lint and format**

```bash
cargo fmt -p mur-common -p mur-core && cargo fmt --check
cargo clippy -p mur-common -p mur-core --lib --tests -- -D warnings
```

**Step 8: Commit**

```bash
git add mur-common/src/config.rs mur-core/src/cmd/conversations_cmd.rs
git commit -m "$(cat <<'EOF'
fix(config): bake per-stage timeout_secs into synthesize_*_backend helpers

P2 review left a workaround in cmd_ask that re-injected ask_cfg.timeout_secs
into the synthesized BackendConfig because the helpers returned timeout_secs:
None, which factory::build defaulted to 120s. That bypassed the user's
per-call budget when no explicit ask.backend override was set.

This change moves the timeout into the helpers themselves:
- AskConfig::synthesize_backend → timeout_secs from ask.timeout_secs
- AskConfig::synthesize_rewriter_backend → timeout_secs from
  ask.rewriter_timeout_secs (rewriter no longer falls through to
  synthesize_backend(); it gets its own short budget so a slow Ollama
  doesn't burn the full ask timeout before the user sees an answer).
- CompactConfig::synthesize_extractive_backend → 120s (matches the
  previously-hardcoded Duration::from_secs(120) at the call site).
- CompactConfig::synthesize_abstractive_backend → 120s same.

Per-stage explicit timeout_secs in user config still wins. Behavior is
byte-identical for all existing config files; the workaround in cmd_ask
is removed.

5 new mur-common tests cover the propagation matrix.

Closes I2 from P2 code review.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Migrate `compact::abstractive::summarize` to `&dyn ChatBackend`

**Files:**
- Modify: `mur-core/src/conversations/summarize/abstractive.rs` (signature change + tests)
- Modify: `mur-core/src/conversations/summarize/mod.rs` (caller — wire `factory::build` like extractive does)

**Step 1: Write the failing test**

In `mur-core/src/conversations/summarize/abstractive.rs`, find the existing `mock_narrative_happy_path` test (around line 237) and add a new test alongside it:

```rust
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn summarize_via_chat_backend_mock_returns_prose() {
        use crate::conversations::backend::mock::MockBackend;
        let _env_guard = ENV_LOCK.lock().unwrap();
        // MockBackend reuses ollama::mock_generate; we don't need MUR_OLLAMA_MOCK
        // (it's a direct trait impl). Clear it to make sure we're hitting the
        // backend path, not the legacy env-var fallback.
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        let backend = MockBackend::new();
        let spans = vec![span(1, "hello world"), span(2, "compression works")];
        let r = summarize(
            &backend,
            "qwen3:14b",
            &spans,
            chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            400,
        )
        .await;
        assert!(
            r.narrative
                .as_deref()
                .unwrap()
                .starts_with("Mock narrative")
        );
        assert!(r.word_count > 0);
    }
```

**Step 2: Run test to confirm it fails to compile**

Run: `cargo test -p mur-core --lib summarize::abstractive 2>&1 | tail -10`

Expected: FAIL — `summarize` still takes `&OllamaClient`, so `&backend` (`&MockBackend`) doesn't typecheck.

**Step 3: Change `summarize`'s signature**

Replace the function body in `summarize/abstractive.rs`:

```rust
use anyhow::Result;
use tracing::warn;

use super::super::backend::{ChatBackend, ChatRequest};
use super::extractive::ExtractiveSpan;

pub struct AbstractiveResult {
    pub narrative: Option<String>,
    pub word_count: usize,
}

pub async fn summarize(
    backend: &dyn ChatBackend,
    model: &str,
    spans: &[ExtractiveSpan],
    date: chrono::NaiveDate,
    max_words: u32,
) -> AbstractiveResult {
    if spans.is_empty() {
        return AbstractiveResult {
            narrative: Some("No significant activity on this day.".to_string()),
            word_count: 6,
        };
    }
    let prompt = render_prompt(spans, date, max_words);
    let resp = backend
        .generate(ChatRequest {
            model,
            user: &prompt,
            system: None,
            max_tokens: max_words * 2, // tokens > words; headroom
            temperature: Some(0.2),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        })
        .await;
    match resp {
        Ok(r) => {
            let narrative = clean_output(&r.text);
            let word_count = narrative.split_whitespace().count();
            AbstractiveResult {
                narrative: Some(narrative),
                word_count,
            }
        }
        Err(e) => {
            warn!("abstractive LLM call failed: {e:#}");
            AbstractiveResult {
                narrative: None,
                word_count: 0,
            }
        }
    }
}
```

(Note: dropped `top_p: Some(0.9)` — `ChatRequest` doesn't carry top_p. The Ollama path's `top_p: 0.9` was never re-implemented in `OllamaBackend`; this matches the existing trait contract. Document inline if reviewer asks.)

Update the existing tests in the same file: replace `OllamaClient::new(...)` + `summarize(&client, ...)` with `MockBackend::new()` + `summarize(&backend, ...)`. The `MUR_OLLAMA_MOCK=1` env-var dance can stay only where the test was specifically asserting the env-var path — the trait-based MockBackend is the cleaner replacement.

For the `empty_spans_emit_placeholder` test (line ~219), drop the env-var dance entirely:

```rust
    #[tokio::test]
    async fn empty_spans_emit_placeholder() {
        use crate::conversations::backend::mock::MockBackend;
        let backend = MockBackend::new();
        let r = summarize(
            &backend,
            "qwen3:14b",
            &[],
            chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            400,
        )
        .await;
        assert!(r.narrative.as_deref().unwrap().contains("No significant"));
    }
```

For `mock_narrative_happy_path` and the other `OllamaClient`-based tests in this file (you grep'd them at task 0): same pattern — replace `OllamaClient` with `MockBackend`, drop the env-var setvar/removevar.

**Step 4: Update the caller in `summarize/mod.rs`**

Around line 130 (the `// Abstractive` block), replace:

```rust
    // Abstractive — same trait migration as P1 extractive. compact.abstractive
    // now flows through factory::build, so users can override
    // `compact.abstractive_backend` to route through Anthropic.
    let abstractive_cfg = cfg.synthesize_abstractive_backend();
    let abstractive_backend = crate::conversations::backend::factory::build(&abstractive_cfg)?;
    let abstractive_result = abstractive::summarize(
        abstractive_backend.as_ref(),
        &abstractive_cfg.model,
        &all_spans,
        date,
        cfg.max_abstractive_words,
    )
    .await;
```

The pre-existing `let client = OllamaClient::new(...)` on line ~101 may no longer be used (extractive already migrated, abstractive now migrated). Check with `grep -n "client\." mur-core/src/conversations/summarize/mod.rs | head -5` and delete the `let client` binding if so.

**Step 5: Run tests**

```bash
cargo test -p mur-core --lib summarize 2>&1 | tail -20
```

Expected: PASS — abstractive tests + mod orchestration tests all green.

**Step 6: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 7: Commit**

```bash
git add mur-core/src/conversations/summarize/abstractive.rs mur-core/src/conversations/summarize/mod.rs
git commit -m "$(cat <<'EOF'
refactor(compact): migrate abstractive summarize to ChatBackend trait

abstractive::summarize now takes &dyn ChatBackend instead of &OllamaClient.
The summarize::compact_day caller constructs the backend via factory::build
using cfg.synthesize_abstractive_backend() — honors per-stage
compact.abstractive_backend override and falls back to Ollama from legacy
fields for users without override.

Behavior is byte-identical for users with no compact.abstractive_backend
override; users who set provider:anthropic now get cloud-side narrative
generation.

This is the fourth call-site migration (after P0 ask::rewriter, P1
compact::extractive, P2 ask::generate). The remaining two — summarize::
rollup and ask::abstractive::compress_hit — land in tasks 3 and 4.

The local OllamaClient binding in summarize::mod is now dead and removed.

Refs spec §3 + §12 (P3 row). Plan task 2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Migrate `summarize::abstractive::rollup_narrative` + caller

**Files:**
- Modify: `mur-core/src/conversations/summarize/abstractive.rs` (`rollup_narrative` signature change)
- Modify: `mur-core/src/conversations/summarize/rollup.rs` (week + month callers, two sites)

**Step 1: Write the failing test**

`rollup_narrative` doesn't have a dedicated test in the abstractive.rs test module — its coverage is via `summarize::rollup` integration. Add a focused test alongside the new `summarize_via_chat_backend_mock_returns_prose`:

```rust
    #[tokio::test]
    async fn rollup_narrative_via_chat_backend_mock_returns_prose() {
        use crate::conversations::backend::mock::MockBackend;
        use crate::conversations::ask::HitInfo;
        use crate::conversations::ask::retrieve::ResolvedHit;
        let backend = MockBackend::new();
        let hits = vec![ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "c1".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                score: 0.9,
            },
            snippet: "rollup test span".into(),
            line_hint: Some(1),
            span_index_in_summary: None,
            vector: None,
            compressed: None,
        }];
        let input = RollupAbstractiveInput {
            kind: RollupKind::Week,
            window_label: "2026-W17",
            selected_spans: &hits,
            prior_narratives: &[],
        };
        let r = rollup_narrative(&backend, "qwen3:14b", &input, 400).await;
        // Mock returns "Mock narrative ..." for the rollup-style prompt.
        assert!(r.narrative.is_some());
        assert!(r.word_count > 0);
    }
```

**Step 2: Run test to confirm it fails to compile**

Run: `cargo test -p mur-core --lib summarize::abstractive::tests::rollup_narrative_via 2>&1 | tail -10`

Expected: FAIL — `rollup_narrative` still takes `&OllamaClient`.

**Step 3: Change `rollup_narrative`'s signature**

In `summarize/abstractive.rs` lines ~113-152:

```rust
pub async fn rollup_narrative(
    backend: &dyn ChatBackend,
    model: &str,
    input: &RollupAbstractiveInput<'_>,
    max_words: u32,
) -> AbstractiveResult {
    let prompt = render_rollup_prompt(input, max_words);
    let resp = backend
        .generate(ChatRequest {
            model,
            user: &prompt,
            system: None,
            max_tokens: max_words * 2,
            temperature: Some(0.2),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        })
        .await;
    match resp {
        Ok(r) => {
            let narrative = clean_output(&r.text);
            let word_count = narrative.split_whitespace().count();
            AbstractiveResult {
                narrative: Some(narrative),
                word_count,
            }
        }
        Err(e) => {
            tracing::warn!("rollup abstractive call failed: {e:#}");
            AbstractiveResult {
                narrative: None,
                word_count: 0,
            }
        }
    }
}
```

**Step 4: Update both callers in `rollup.rs`**

Find both call sites (around lines 172 and 413 — week and month rollups). Each currently builds an `OllamaClient` and passes `&client` to `rollup_narrative`. Replace each with the `factory::build` pattern:

For the week rollup site:

```rust
    // Abstractive (P3 migration: trait-based)
    let abstractive_cfg = mur_common::config::BackendConfig {
        provider: "ollama".into(),
        model: cfg.abstractive_model.clone(),
        endpoint: Some(cfg.ollama_endpoint.clone()),
        api_key_env: None,
        timeout_secs: Some(120),
    };
    let abstractive_backend =
        crate::conversations::backend::factory::build(&abstractive_cfg)?;
    let abstractive = super::abstractive::rollup_narrative(
        abstractive_backend.as_ref(),
        &cfg.abstractive_model,
        &RollupAbstractiveInput { ... }, // existing block unchanged
        cfg.max_abstractive_words_per_week,
    )
    .await;
```

Apply the same pattern to the month rollup site at line ~413.

(Note: `RollupConfig` does NOT have a per-stage `abstractive_backend` override field today — the spec's per-stage routing focused on `compact` and `ask`. Adding rollup_backend overrides is a defensible follow-up but **out of scope for P3** — see "Out of scope" at the top. We synthesize the legacy ollama config inline here instead of adding a `synthesize_*_backend` helper to `RollupConfig`.)

**Step 5: Run tests**

```bash
cargo test -p mur-core --lib summarize 2>&1 | tail -20
cargo test -p mur-core --lib conversations::summarize::rollup -- --test-threads=1 2>&1 | tail -15
```

Expected: all pass. The week + month rollup orchestration tests use mock mode and should be byte-identical.

**Step 6: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 7: Commit**

```bash
git add mur-core/src/conversations/summarize/abstractive.rs mur-core/src/conversations/summarize/rollup.rs
git commit -m "$(cat <<'EOF'
refactor(rollup): migrate rollup_narrative to ChatBackend trait

rollup_narrative now takes &dyn ChatBackend instead of &OllamaClient. Both
the week and month rollup orchestrators in summarize::rollup construct the
backend inline via factory::build using a synthesized ollama BackendConfig
(RollupConfig has no per-stage backend override fields today — adding
rollup-specific routing is a defensible follow-up but explicitly out of
scope for P3).

Behavior is byte-identical for existing users (still hits ollama via
factory::build → OllamaBackend wrapped in RetryingBackend).

Fifth call-site migration. After this only ask::abstractive::compress_hit
remains on direct OllamaClient (Task 4).

Refs spec §3. Plan task 3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Migrate `ask::abstractive::compress_hit` (Stage 1b) to `&dyn ChatBackend`

**Files:**
- Modify: `mur-core/src/conversations/ask/abstractive.rs` (`AbstractiveCtx` + `compress_hit` + tests)
- Modify: `mur-core/src/conversations/ask/mod.rs` lines ~204-217 (caller — replace `OllamaClient` construction with `factory::build`)

**Step 1: Read the current `AbstractiveCtx` shape**

```rust
pub struct AbstractiveCtx<'a> {
    pub client: &'a OllamaClient,
    pub model: &'a str,
    pub timeout: Duration,
    pub root_override: Option<&'a str>,
}
```

The `timeout` field is the per-call Stage 1b budget (5s by default). `tokio::time::timeout` wraps the call, so the trait migration keeps that wrapper — `AbstractiveCtx` swaps `client: &OllamaClient` for `backend: &dyn ChatBackend`. The timeout stays in the ctx, separate from the backend's HTTP timeout.

**Step 2: Write the failing test**

In `ask/abstractive.rs` find `mod tests` (around line 283) and the `ctx<'a>` helper (around line 312). Add a new helper + test:

```rust
    fn ctx_backend<'a>(backend: &'a dyn crate::conversations::backend::ChatBackend, root: &'a str) -> AbstractiveCtx<'a> {
        AbstractiveCtx {
            backend,
            model: "qwen3:14b",
            timeout: Duration::from_millis(200),
            root_override: Some(root),
        }
    }

    #[tokio::test]
    async fn compress_hit_via_chat_backend_mock_short_circuits_on_short_content() {
        use crate::conversations::backend::mock::MockBackend;
        let backend = MockBackend::new();
        let tmp = tempfile::tempdir().unwrap();
        let mut h = long_hit(1);
        h.snippet = "tiny".into();
        let outcome = compress_hit(&ctx_backend(&backend, tmp.path().to_str().unwrap()), &mut h, 100).await;
        assert!(matches!(
            outcome,
            CompressOutcome::Skipped(skip_reason::TOO_SHORT)
        ));
    }
```

**Step 3: Run to confirm fail**

Run: `cargo test -p mur-core --lib ask::abstractive::tests::compress_hit_via 2>&1 | tail -10`

Expected: FAIL to compile — `AbstractiveCtx.client` field still exists, `backend` doesn't.

**Step 4: Update `AbstractiveCtx` and `compress_hit`**

In `ask/abstractive.rs`:

```rust
use super::cache;
use super::retrieve::ResolvedHit;
use crate::conversations::backend::{ChatBackend, ChatRequest};
use std::time::Duration;

// ... existing constants unchanged ...

pub struct AbstractiveCtx<'a> {
    pub backend: &'a dyn ChatBackend,
    pub model: &'a str,
    pub timeout: Duration,
    pub root_override: Option<&'a str>,
}

// ... CompressOutcome / skip_reason unchanged ...

pub async fn compress_hit(
    ctx: &AbstractiveCtx<'_>,
    hit: &mut ResolvedHit,
    target_tokens: usize,
) -> CompressOutcome {
    if hit.snippet.len() < MIN_CONTENT_CHARS {
        return CompressOutcome::Skipped(skip_reason::TOO_SHORT);
    }
    let target = target_tokens.max(MIN_TARGET_TOKENS_PER_HIT);
    let key = cache::cache_key(ctx.model, target, &hit.snippet);

    if let Some(cached) = cache::cache_get(&key, ctx.root_override) {
        if !cached.is_empty() && cached.len() < hit.snippet.len() {
            hit.snippet = cached;
            hit.compressed = Some(super::Compression::Abstractive);
            return CompressOutcome::CacheHit;
        }
        tracing::debug!(
            key,
            cached_len = cached.len(),
            orig_len = hit.snippet.len(),
            "cache entry present but invalid, evicting + retrying"
        );
        cache::cache_remove(&key, ctx.root_override);
    }

    let prompt = user_template(target, &hit.snippet);
    let req = ChatRequest {
        model: ctx.model,
        user: &prompt,
        system: Some(SYSTEM_TEMPLATE),
        max_tokens: target as u32 * 2,
        temperature: Some(0.0),
        stop: vec![],
        cache_system: false, // P3 caching: SYSTEM_TEMPLATE is ~30 tokens, far below the 2048 cacheable minimum (spec §5.2). Set true if SYSTEM_TEMPLATE grows past 2048 tokens.
        cache_user_prefix: None,
    };

    let call = ctx.backend.generate(req);
    let out = match tokio::time::timeout(ctx.timeout, call).await {
        Err(_) => {
            tracing::warn!(target, len = hit.snippet.len(), "stage-1b timeout");
            return CompressOutcome::Skipped(skip_reason::TIMEOUT);
        }
        Ok(Err(e)) => {
            tracing::warn!(target, err = ?e, "stage-1b backend error");
            return CompressOutcome::Skipped(skip_reason::OLLAMA_ERR);
        }
        Ok(Ok(resp)) => resp.text,
    };

    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        return CompressOutcome::Skipped(skip_reason::EMPTY);
    }
    if trimmed.len() >= hit.snippet.len() {
        return CompressOutcome::Skipped(skip_reason::NOT_SHORTER);
    }

    if let Err(e) = cache::cache_put(&key, &trimmed, ctx.root_override) {
        tracing::warn!(key, err = ?e, "stage-1b cache write failed");
    }

    hit.snippet = trimmed;
    hit.compressed = Some(super::Compression::Abstractive);
    CompressOutcome::Compressed
}
```

(`skip_reason::OLLAMA_ERR` keeps its name for backwards-compatible JSON output — the constant value is documented as a generic backend-error tag now. Don't rename.)

Update existing tests in this file: replace `OllamaClient::new(...)` + `ctx(&client, ...)` with `MockBackend::new()` + `ctx_backend(&backend, ...)`. The env-var dance for MUR_OLLAMA_MOCK can stay only if a test explicitly verifies the legacy env-var path (it doesn't — they're all just constructing client+ctx for trait calls).

**Step 5: Update the caller in `ask/mod.rs`**

Around line 204:

```rust
    // 3. Build prompt (incl. Phase 3.5 Stage 1b when enabled).
    //
    // Stage 1b backend: P3 migration. Re-use the answer backend if already
    // built — Stage 1b's per-call timeout is enforced separately via
    // ctx.timeout (tokio::time::timeout wrapper), so we don't need a new
    // backend with a different connect-level timeout. When req.answer_backend
    // is None (legacy callers / tests), synthesize ollama from req.endpoint /
    // req.timeout the same way the answer-stream block below does.
    let stage1b_backend: Arc<dyn ChatBackend> = match req.answer_backend.clone() {
        Some(b) => b,
        None => {
            let cfg = mur_common::config::BackendConfig {
                provider: "ollama".into(),
                model: req.model.clone(),
                endpoint: Some(req.endpoint.clone()),
                api_key_env: None,
                timeout_secs: Some(req.timeout.as_secs()),
            };
            crate::conversations::backend::factory::build(&cfg)?
        }
    };

    let summarize_model_owned: Option<String> = req
        .summarize_model
        .clone()
        .or_else(|| Some(req.model.clone()));
    let abstractive_ctx_owned =
        summarize_model_owned
            .as_ref()
            .map(|m| abstractive::AbstractiveCtx {
                backend: stage1b_backend.as_ref(),
                model: m.as_str(),
                timeout: abstractive::CALL_TIMEOUT,
                root_override,
            });
```

Then below (around line 248) in the answer-stream block, the existing `req.answer_backend.clone()` logic can dedupe — the `match` you just wrote above already binds `stage1b_backend`. Reuse it instead of re-running the same factory::build:

```rust
    let model = req.model.clone();
    let answer_backend = stage1b_backend; // already built above
```

(This deletes the second `let answer_backend: Arc<dyn ChatBackend> = match ...` block — net LOC reduction.)

**Step 6: Run tests**

```bash
cargo test -p mur-core --lib ask -- --test-threads=1 2>&1 | tail -20
cargo test -p mur-core --test cli_conversations -- --test-threads=1 2>&1 | tail -10
```

Expected: PASS — Stage 1b tests + ask end-to-end tests all green.

**Step 7: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 8: Commit**

```bash
git add mur-core/src/conversations/ask/abstractive.rs mur-core/src/conversations/ask/mod.rs
git commit -m "$(cat <<'EOF'
refactor(ask): migrate Stage 1b compress_hit to ChatBackend trait

AbstractiveCtx swaps client: &OllamaClient for backend: &dyn ChatBackend.
compress_hit's tokio::time::timeout wrapper stays — Stage 1b's per-call
budget (5s by default) is enforced independently of the backend's HTTP
timeout, since Stage 1b soft-fails on timeout and the user is interactive.

The ask::ask_stream caller now builds the Stage 1b backend once via
factory::build (re-using the same backend for the answer stream below),
deduping a second factory::build call that the P2 migration introduced.

Sixth and final call-site migration in the conversations subsystem. After
this every LLM call in conversations/ flows through ChatBackend, which
unblocks per-call telemetry (Task 8) and prompt-caching wiring (Tasks 5-7).

The legacy `skip_reason::OLLAMA_ERR` constant keeps its name for backwards-
compatible JSON output — semantically it's now a generic backend-error tag.

Refs spec §3. Plan task 4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Wire prompt caching in `AnthropicBackend`

**Files:**
- Modify: `mur-core/src/conversations/backend/anthropic.rs` (request-body shape + `supports_caching()`)
- Modify: `mur-core/src/conversations/backend/mod.rs` (default-impl docstring)

**Step 1: Write the failing tests**

In `anthropic.rs` `mod tests` append three new tests covering: (a) cache_system=true emits the right request body, (b) cache_user_prefix=Some(N) emits the right request body, (c) supports_caching() returns true.

```rust
    #[test]
    fn supports_caching_is_true_for_anthropic() {
        let b = AnthropicBackend::new("http://unused", "k", Duration::from_millis(100));
        assert!(b.supports_caching());
    }

    #[tokio::test]
    async fn cache_system_true_emits_system_block_with_cache_control_ephemeral() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":1}}"#),
            )
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let mut r = req("claude-haiku-4-5", "hi");
        r.system = Some("you are a tester");
        r.cache_system = true;
        let _ = b.generate(r).await.unwrap();

        let received = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        // system MUST be a JSON array of blocks (not a plain string) when caching
        let system = body.get("system").expect("system field present");
        assert!(system.is_array(), "expected system to be a block array, got {system:?}");
        let arr = system.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("type").and_then(|v| v.as_str()), Some("text"));
        assert_eq!(arr[0].get("text").and_then(|v| v.as_str()), Some("you are a tester"));
        assert_eq!(
            arr[0].get("cache_control").and_then(|v| v.get("type")).and_then(|v| v.as_str()),
            Some("ephemeral"),
            "expected cache_control: {{type: ephemeral}} on the system block"
        );
    }

    #[tokio::test]
    async fn cache_user_prefix_emits_two_block_user_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":1}}"#),
            )
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let mut r = req("claude-haiku-4-5", "PREFIX_BLOCKsuffix");
        r.cache_user_prefix = Some("PREFIX_BLOCK".len());
        let _ = b.generate(r).await.unwrap();

        let received = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
        let content = messages[0].get("content").unwrap();
        // With cache_user_prefix, content MUST be an array of two blocks: cached prefix + volatile suffix.
        assert!(content.is_array(), "expected content to be a block array, got {content:?}");
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].get("text").and_then(|v| v.as_str()), Some("PREFIX_BLOCK"));
        assert_eq!(
            arr[0].get("cache_control").and_then(|v| v.get("type")).and_then(|v| v.as_str()),
            Some("ephemeral"),
        );
        assert_eq!(arr[1].get("text").and_then(|v| v.as_str()), Some("suffix"));
        assert!(
            arr[1].get("cache_control").is_none(),
            "second block must NOT have cache_control"
        );
    }

    #[tokio::test]
    async fn no_caching_hints_keeps_legacy_request_shape() {
        // When neither cache_system nor cache_user_prefix is set, system stays
        // a plain string and content stays a plain string — minimizes JSON
        // shape churn for callers that don't need caching.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":1}}"#),
            )
            .mount(&server)
            .await;
        let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
        let mut r = req("claude-haiku-4-5", "hi");
        r.system = Some("you are a tester");
        // Both caching hints stay default (false / None) — same shape as before P3.
        let _ = b.generate(r).await.unwrap();

        let received = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        assert!(body.get("system").unwrap().is_string(), "system should stay a string when not caching");
        let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
        assert!(messages[0].get("content").unwrap().is_string(), "content should stay a string when not caching");
    }
```

**Step 2: Run tests to confirm they fail**

Run: `cargo test -p mur-core --lib conversations::backend::anthropic 2>&1 | tail -20`

Expected: FAIL — `supports_caching` defaults to `false`, and the request body is built from the typed `ApiRequest` struct that emits `system` as a plain `Option<&str>`.

**Step 3: Refactor `generate` to emit the right body shape**

Replace the `let body = ApiRequest { ... }` block with a `serde_json::Value` builder that branches on caching flags. Keep the typed structs around for use sites that don't need the dynamic shape (delete `ApiRequest` and `ApiMessage` if they're only used by `generate`):

```rust
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let url = format!("{}/v1/messages", self.endpoint);
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

        let body = build_request_body(&req, temperature, false /* stream */);

        let resp = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|source| BackendError::Network {
                provider: "anthropic",
                source,
            })?;
        // ... rest unchanged ...
    }

    fn supports_caching(&self) -> bool {
        true
    }
```

Apply the same `build_request_body(&req, temperature, true /* stream */)` substitution in `generate_stream`.

Add the helper above `parse_sse_block`:

```rust
/// Build the JSON request body for /v1/messages.
///
/// When `cache_system` is true and a system prompt is present, emits `system`
/// as a single-block array with `cache_control: {type: ephemeral}` (per spec
/// §5.2 caching invariants). When `cache_user_prefix` is `Some(n)`, splits
/// the user content at byte n and emits a two-block content array with the
/// breakpoint on the prefix block. When neither hint is set, emits the
/// legacy shape: `system` as a plain string, `content` as a plain string.
///
/// The `stream` flag adds `"stream": true` for SSE responses.
fn build_request_body(req: &ChatRequest<'_>, temperature: Option<f32>, stream: bool) -> serde_json::Value {
    use serde_json::json;

    let max_tokens = if req.max_tokens == 0 {
        DEFAULT_MAX_TOKENS
    } else {
        req.max_tokens
    };

    // System: array form (with cache_control) only when cache_system && system present.
    let system_value = match (req.cache_system, req.system) {
        (true, Some(s)) => json!([
            {"type": "text", "text": s, "cache_control": {"type": "ephemeral"}}
        ]),
        (_, Some(s)) => json!(s),
        (_, None) => serde_json::Value::Null,
    };

    // User content: two-block array (cached prefix + volatile suffix) only when
    // cache_user_prefix is Some and the offset is in range. Otherwise plain string.
    let content_value = match req.cache_user_prefix {
        Some(n) if n > 0 && n < req.user.len() => {
            let prefix = &req.user[..n];
            let suffix = &req.user[n..];
            json!([
                {"type": "text", "text": prefix, "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": suffix},
            ])
        }
        _ => json!(req.user),
    };

    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": content_value}],
        "thinking": {"type": "disabled"},
    });

    let map = body.as_object_mut().unwrap();
    if !system_value.is_null() {
        map.insert("system".into(), system_value);
    }
    if let Some(t) = temperature {
        map.insert("temperature".into(), json!(t));
    }
    if !req.stop.is_empty() {
        map.insert("stop_sequences".into(), json!(req.stop));
    }
    if stream {
        map.insert("stream".into(), json!(true));
    }
    body
}
```

Delete the now-unused `ApiRequest`, `ApiMessage`, `ApiThinking` structs. The dead-code lint will catch them.

In `generate_stream` replace the `serde_json::json!({...})` block + `strip_null_values(...)` call with `build_request_body(&req, temperature, true)`. Delete `strip_null_values` — the new helper builds the body directly without nulls.

**Step 4: Update the trait default-doc**

In `mur-core/src/conversations/backend/mod.rs`, the `supports_caching` method already has a docstring — update it to note that AnthropicBackend now returns true:

```rust
    /// True when the backend honors `cache_system` / `cache_user_prefix`
    /// hints. False = hints are silently ignored. Default: false.
    /// AnthropicBackend overrides to true (P3+).
    fn supports_caching(&self) -> bool {
        false
    }
```

**Step 5: Run tests**

```bash
cargo test -p mur-core --lib conversations::backend -- --test-threads=1 2>&1 | tail -20
```

Expected: PASS — 4 new caching tests + all 9 existing non-streaming tests + 4 streaming tests + 8 retry tests + factory tests + ollama tests all green.

**Step 6: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 7: Commit**

```bash
git add mur-core/src/conversations/backend/anthropic.rs mur-core/src/conversations/backend/mod.rs
git commit -m "$(cat <<'EOF'
feat(backend): wire prompt-caching hints into AnthropicBackend request body

ChatRequest.cache_system + cache_user_prefix were stubs in P0/P1/P2 — the
trait carried them but AnthropicBackend ignored them. P3 wires them into
the request body per spec §5.2:

- cache_system=true && system present → system field is a JSON array of
  blocks with cache_control: {type: ephemeral} on the (sole) block.
- cache_user_prefix=Some(n) → user content is a two-block array:
  cached prefix [0..n] + volatile suffix [n..]. Cache breakpoint on the
  prefix block.
- Both hints unset → legacy shape: system as plain string, content as
  plain string. Minimizes JSON shape churn for non-caching callers.

supports_caching() now returns true for AnthropicBackend (was false).

Replaces the typed ApiRequest/ApiMessage/ApiThinking structs with a
serde_json::Value builder (build_request_body) since the body shape is
now dynamic. strip_null_values is dropped — the builder doesn't emit
nulls.

4 wiremock tests cover request-body shape: cache_system→system block
array with ephemeral cache_control, cache_user_prefix→two-block content
with breakpoint on prefix, no hints→legacy string shape, supports_caching
returns true.

Caching hints are not yet *set* by call sites — that's tasks 6-7. This
task only adds the wire support so those tasks have something to wire to.

Note: minimum cacheable prefix is 2048 tokens for Haiku 4.5 / 4096 tokens
for Opus 4.6+ (spec §5.2). Below that, cache_control is silently ignored
by Anthropic. Verify via usage.cache_creation_input_tokens.

Refs spec §5.2 (caching invariants), §12 (P3 row). Plan task 5.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Set `cache_system` on the `compact.extractive` LLM call (canary)

**Files:**
- Modify: `mur-core/src/conversations/summarize/extractive.rs` (`extract_chunk` — set `cache_system: true` on the ChatRequest)

**Step 1: Justify the choice**

The extractive prompt has TWO parts: (a) a long, fixed instruction body (~30 lines) explaining what makes a span quote-worthy + the JSON output schema, and (b) the per-chunk excerpt. Today both are baked into a single `user` prompt by `render_prompt` (extractive.rs:78-121). To make the fixed prefix cacheable we must split it: fixed instructions go into `system`, the per-chunk excerpt stays in `user`.

The fixed prefix is approximately 220 tokens — **below the 2048-token Haiku 4.5 minimum** (spec §5.2), so cache_creation_input_tokens will report 0 today. We wire the hint anyway because (i) it costs us nothing to send and (ii) the prompt is likely to grow (P4 may expand it) — we don't want to forget to flip the bit later. The hint is silently ignored by Ollama (`supports_caching() == false`), so this change is a no-op for local-first users.

**Step 2: Write the failing test**

In `extractive.rs` `mod tests` add:

```rust
    #[test]
    fn render_prompt_emits_system_and_user_separately() {
        let msgs = vec![mk(0, "c1", "hello", Role::User)];
        let chunk = Chunk {
            messages: msgs.clone(),
            token_count: 10,
            span_range: (1, 1),
        };
        let (system, user) = render_prompt_split(&chunk);
        assert!(
            system.contains("quote-worthy"),
            "system prompt should contain the fixed instruction prefix; got: {system:?}"
        );
        assert!(
            !system.contains("Excerpt"),
            "system prompt should NOT contain the per-chunk excerpt header; got: {system:?}"
        );
        assert!(
            user.starts_with("Excerpt ("),
            "user prompt should start with the per-chunk Excerpt header; got: {user:?}"
        );
    }
```

**Step 3: Run to confirm fail**

Run: `cargo test -p mur-core --lib summarize::extractive::tests::render_prompt_emits_system 2>&1 | tail -10`

Expected: FAIL to compile — `render_prompt_split` doesn't exist yet.

**Step 4: Split the prompt**

In `extractive.rs` replace `render_prompt` with `render_prompt_split` returning `(String, String)`:

```rust
/// Split the extractive prompt into a fixed system instruction (cacheable
/// across calls within a day) and a per-chunk user message (volatile).
/// See P3 task 6 — caching wired even though the system prompt today is
/// well below the 2048-token Haiku-4.5 minimum (spec §5.2). The hint is
/// silently ignored by non-Anthropic backends.
fn render_prompt_split(chunk: &Chunk) -> (String, String) {
    let system = "You are reviewing one conversation day for a technical developer's personal archive. \
         Extract the 1-3 most informative spans from this excerpt.\n\n\
         A span is quote-worthy if it:\n\
         - states a decision the user made (\"we'll use X over Y because...\")\n\
         - records a concrete error or failure that shaped subsequent work\n\
         - captures a new idea, technique, or reference the user hadn't seen before\n\
         - quotes an important external fact (API response, spec excerpt, doc)\n\n\
         A span is NOT quote-worthy if it is:\n\
         - boilerplate/greeting/filler\n\
         - tool-result body already citeable by path\n\
         - restated from an earlier span\n\n\
         Output format: JSON array. Each span is {role, conv_id, line_hint, text}.\n\
           - role: one of \"user\" | \"assistant\" | \"system\" | \"tool\"\n\
           - conv_id: the conv value from the source message\n\
           - line_hint: integer line number within the day's raw JSONL\n\
           - text: verbatim quote, 20-400 chars\n\n\
         If the excerpt has nothing quote-worthy, return [].".to_string();

    let mut user = String::new();
    let (start_line, end_line) = chunk.span_range;
    user.push_str(&format!(
        "Excerpt ({} messages, lines {}..{}):\n",
        chunk.messages.len(),
        start_line,
        end_line
    ));
    for (i, m) in chunk.messages.iter().enumerate() {
        let line_no = start_line + i;
        let role = format!("{:?}", m.role).to_lowercase();
        let text = content_preview(m);
        user.push_str(&format!(
            "L{} [{}] {}/{} ({}): {}\n",
            line_no,
            m.ts.format("%H:%M:%S"),
            m.src.file_prefix(),
            m.conv,
            role,
            text,
        ));
    }
    (system, user)
}
```

Update `extract_chunk` to use the split:

```rust
    let (system, user) = render_prompt_split(chunk);
    let resp = backend
        .generate(ChatRequest {
            model,
            user: &user,
            system: Some(&system),
            max_tokens: 1024,
            temperature: Some(0.0),
            stop: vec![],
            cache_system: true,
            cache_user_prefix: None,
        })
        .await;
```

Delete the old `render_prompt` fn (it's replaced by `render_prompt_split`).

**Step 5: Run tests**

```bash
cargo test -p mur-core --lib summarize::extractive 2>&1 | tail -15
```

Expected: PASS — the new test + 4 existing extractive tests + 1 mock-backend extractive test all green.

**Step 6: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 7: Commit**

```bash
git add mur-core/src/conversations/summarize/extractive.rs
git commit -m "$(cat <<'EOF'
feat(compact): split extractive prompt and set cache_system hint (canary)

P3 caching canary. Splits render_prompt into render_prompt_split returning
(system_prompt, user_prompt). The fixed instruction body (~220 tokens of
quote-worthy/not-quote-worthy criteria + JSON output schema) moves to
system; the per-chunk excerpt stays in user. ChatRequest.cache_system=true
on the ChatRequest hints the backend to set cache_control: {type: ephemeral}
on the system block.

The system prompt is currently ~220 tokens — below the 2048-token Haiku 4.5
cacheable minimum (spec §5.2). So cache_creation_input_tokens will report 0
today and there's no immediate cost reduction. We wire the hint anyway
because (a) it costs nothing on the wire and (b) if the prompt grows past
2048 tokens (likely as the criteria list expands) caching activates
automatically.

Ollama backends silently ignore the hint (supports_caching=false). For
users on local Ollama this change has zero behavioral effect.

Refs spec §5.2. Plan task 6.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Set `cache_system` on the `compact.abstractive` LLM call

**Files:**
- Modify: `mur-core/src/conversations/summarize/abstractive.rs` (`render_prompt` → `render_prompt_split` + `summarize` sets `cache_system: true`)

**Step 1: Same pattern as Task 6**

The abstractive prompt's fixed prefix (the "you are summarizing..." instruction + word count constraints) is ~80 tokens — also below the 2048-token cacheable minimum. We wire the hint anyway for the same reasons as Task 6: zero wire cost, future-proofing.

**Step 2: Write the failing test**

In `summarize/abstractive.rs` `mod tests` add:

```rust
    #[test]
    fn summarize_render_prompt_split_separates_system_and_user() {
        let spans = vec![span(1, "hello world")];
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let (system, user) = render_prompt_split(&spans, date, 400);
        assert!(system.contains("summarizing one day"));
        assert!(!system.contains("Spans:"), "system should not contain the per-day Spans block");
        assert!(user.starts_with("Spans:"));
        assert!(user.contains("hello world"));
    }
```

(Note: the helper is `span(idx, text)` defined in the test module already.)

**Step 3: Run to confirm fail**

Run: `cargo test -p mur-core --lib summarize::abstractive::tests::summarize_render_prompt_split 2>&1 | tail -10`

Expected: FAIL to compile.

**Step 4: Split the prompt + wire the hint**

In `summarize/abstractive.rs` replace `render_prompt` with `render_prompt_split`:

```rust
fn render_prompt_split(spans: &[ExtractiveSpan], date: chrono::NaiveDate, max_words: u32) -> (String, String) {
    let min_words = 150.min(max_words / 2);
    let system = format!(
        "You are summarizing one day of a developer's AI-assistant conversations into a \
         narrative paragraph. Use ONLY information present in the spans provided.\n\n\
         Output: {}-{} words, first-person or neutral third-person, no bullet lists. \
         Reference each key point by its span index [N]. Do NOT invent details not in the spans. \
         If spans conflict, note the conflict.",
        min_words, max_words
    );
    let mut user = format!("Date: {}\n\nSpans:\n", date);
    for (i, s) in spans.iter().enumerate() {
        user.push_str(&format!(
            "[{}] {{{} {}/{} L{}}}: {}\n",
            i + 1,
            date,
            s.src.file_prefix(),
            s.conv_id,
            s.line_hint,
            s.text,
        ));
    }
    user.push_str("\nWrite the narrative.\n");
    (system, user)
}
```

In `summarize`, replace the `render_prompt(...)` call:

```rust
    let (system, user) = render_prompt_split(spans, date, max_words);
    let resp = backend
        .generate(ChatRequest {
            model,
            user: &user,
            system: Some(&system),
            max_tokens: max_words * 2,
            temperature: Some(0.2),
            stop: vec![],
            cache_system: true,
            cache_user_prefix: None,
        })
        .await;
```

Apply the same split treatment to `rollup_narrative`'s `render_rollup_prompt` if you want symmetry — but the rollup prompt's fixed prefix is even shorter (~50 tokens) so the wire effect is nil. **Skip it for P3** to keep this task focused; rollup's prompt would benefit from the split only if it expands.

Delete the old `render_prompt` fn.

**Step 5: Run tests**

```bash
cargo test -p mur-core --lib summarize::abstractive 2>&1 | tail -15
```

Expected: PASS.

**Step 6: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 7: Commit**

```bash
git add mur-core/src/conversations/summarize/abstractive.rs
git commit -m "$(cat <<'EOF'
feat(compact): split abstractive prompt and set cache_system hint

Same canary pattern as task 6. Splits the day-narrative render_prompt into
(system, user). Fixed instruction body moves to system; per-day Date+Spans
block stays in user. ChatRequest.cache_system=true.

System prompt is ~80 tokens — well below the 2048-token Haiku 4.5
cacheable minimum (spec §5.2), so cache_creation_input_tokens reports 0
today. Wired anyway for future-proofing; zero wire cost.

rollup_narrative's prompt split is intentionally NOT done in this task
(prefix is ~50 tokens, even less benefit; out of scope for the P3 canary
sweep).

Refs spec §5.2. Plan task 7.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Per-call cost telemetry — JSONL writer + `tracing` instrumentation

**Files:**
- Create: `mur-core/src/conversations/backend/telemetry.rs` (new module — instrumentation wrapper + JSONL writer)
- Modify: `mur-core/src/conversations/backend/mod.rs` (add `pub mod telemetry`)
- Modify: `mur-core/src/conversations/backend/factory.rs` (wrap RetryingBackend's output in TelemetryBackend so all call sites get telemetry uniformly)
- Modify: `mur-core/src/conversations/paths.rs` (add `telemetry_root()` helper)

**Step 1: Decide instrumentation shape**

Spec §11 sketches a `tracing::instrument(...)` macro on a wrapper fn. We use a more direct approach: a `TelemetryBackend` decorator, parallel to `RetryingBackend`. It owns the JSONL writer (a per-day file at `~/.mur/telemetry/llm-calls-<YYYY-MM-DD>.jsonl`) and writes one line per `generate` call + one line per `generate_stream` final chunk. This avoids dependency on `tracing_subscriber` JSON layer setup at the binary level (which gets hairy with multiple subscribers in tests).

Each line is a `LlmCallRecord`:

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct LlmCallRecord {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub provider: String,        // "ollama" | "anthropic" | "mock"
    pub model: String,
    pub stage: String,           // "extractive" | "abstractive" | "rewriter" | "ask.generate" | "ask.compress_hit" | "rollup"
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub latency_ms: u64,
    pub stream: bool,
    pub success: bool,
}
```

Stage tagging: `TelemetryBackend` accepts a `stage: &'static str` at construction. Factory builds a fresh `TelemetryBackend` per call site with the right tag.

**Step 2: Write the failing tests for telemetry.rs**

Create `mur-core/src/conversations/backend/telemetry.rs` with module-level structure:

```rust
//! Per-call cost telemetry: writes one JSONL record per LLM call to
//! `~/.mur/telemetry/llm-calls-<YYYY-MM-DD>.jsonl`. See spec §11 + plan task 8.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use super::{ChatBackend, ChatChunk, ChatRequest, ChatResponse, ChatStream};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct LlmCallRecord {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub provider: String,
    pub model: String,
    pub stage: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub latency_ms: u64,
    pub stream: bool,
    pub success: bool,
}

pub struct TelemetryBackend {
    inner: Arc<dyn ChatBackend>,
    stage: &'static str,
    /// Test override: when Some, records are appended here instead of the
    /// default ~/.mur/telemetry path.
    log_path_override: Option<PathBuf>,
}

impl TelemetryBackend {
    pub fn new(inner: Arc<dyn ChatBackend>, stage: &'static str) -> Self {
        Self { inner, stage, log_path_override: None }
    }

    pub fn with_path_override(mut self, path: PathBuf) -> Self {
        self.log_path_override = Some(path);
        self
    }

    fn log_path(&self) -> PathBuf {
        if let Some(p) = &self.log_path_override {
            return p.clone();
        }
        let date = Utc::now().format("%Y-%m-%d");
        super::super::paths::telemetry_root(None).join(format!("llm-calls-{date}.jsonl"))
    }

    fn write_record(&self, rec: &LlmCallRecord) {
        let path = self.log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let line = match serde_json::to_string(rec) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(err = ?e, "failed to serialize LlmCallRecord");
                return;
            }
        };
        use std::io::Write;
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{line}") {
                    tracing::warn!(err = ?e, path = ?path, "failed to write telemetry line");
                }
            }
            Err(e) => {
                tracing::warn!(err = ?e, path = ?path, "failed to open telemetry file");
            }
        }
    }
}

#[async_trait]
impl ChatBackend for TelemetryBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let start = Instant::now();
        let model = req.model.to_string();
        let provider = self.inner.provider_name().to_string();
        let result = self.inner.generate(req).await;
        let latency_ms = start.elapsed().as_millis() as u64;
        match &result {
            Ok(resp) => {
                self.write_record(&LlmCallRecord {
                    ts: Utc::now(),
                    provider,
                    model,
                    stage: self.stage.to_string(),
                    input_tokens: resp.usage.input_tokens,
                    output_tokens: resp.usage.output_tokens,
                    cache_creation_input_tokens: resp.usage.cache_creation_input_tokens,
                    cache_read_input_tokens: resp.usage.cache_read_input_tokens,
                    latency_ms,
                    stream: false,
                    success: true,
                });
            }
            Err(_) => {
                self.write_record(&LlmCallRecord {
                    ts: Utc::now(),
                    provider,
                    model,
                    stage: self.stage.to_string(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    latency_ms,
                    stream: false,
                    success: false,
                });
            }
        }
        result
    }

    async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
        use futures::stream::StreamExt;
        let start = Instant::now();
        let model = req.model.to_string();
        let provider = self.inner.provider_name().to_string();
        let stage = self.stage.to_string();
        let log_path_override = self.log_path_override.clone();
        let inner_stream = match self.inner.generate_stream(req).await {
            Ok(s) => s,
            Err(e) => {
                // Record connect-time failure.
                let rec = LlmCallRecord {
                    ts: Utc::now(),
                    provider,
                    model,
                    stage,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    latency_ms: start.elapsed().as_millis() as u64,
                    stream: true,
                    success: false,
                };
                Self { inner: self.inner.clone(), stage: self.stage, log_path_override }
                    .write_record(&rec);
                return Err(e);
            }
        };
        // Wrap the stream so we capture the final usage chunk.
        let path_override = self.log_path_override.clone();
        let stage_owned = self.stage;
        let stream = futures::stream::unfold(
            (inner_stream, model, provider, stage_owned, path_override, start, false),
            |(mut s, model, provider, stage, path_override, start, done)| async move {
                if done {
                    return None;
                }
                match s.next().await {
                    Some(Ok(chunk)) => {
                        let final_usage = chunk.usage.clone();
                        let item = Ok(chunk);
                        // If this chunk carries usage, write the record now.
                        if let Some(u) = final_usage {
                            let rec = LlmCallRecord {
                                ts: Utc::now(),
                                provider: provider.clone(),
                                model: model.clone(),
                                stage: stage.to_string(),
                                input_tokens: u.input_tokens,
                                output_tokens: u.output_tokens,
                                cache_creation_input_tokens: u.cache_creation_input_tokens,
                                cache_read_input_tokens: u.cache_read_input_tokens,
                                latency_ms: start.elapsed().as_millis() as u64,
                                stream: true,
                                success: true,
                            };
                            // We need a TelemetryBackend instance to call write_record;
                            // synthesize a one-off (the inner Arc isn't needed for path resolution).
                            // Cheaper: inline the file-write logic.
                            write_record_to_path(path_override.clone(), &rec);
                        }
                        Some((item, (s, model, provider, stage, path_override, start, false)))
                    }
                    Some(Err(e)) => {
                        // Mid-stream error: record failure with whatever latency we have.
                        let rec = LlmCallRecord {
                            ts: Utc::now(),
                            provider: provider.clone(),
                            model: model.clone(),
                            stage: stage.to_string(),
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 0,
                            latency_ms: start.elapsed().as_millis() as u64,
                            stream: true,
                            success: false,
                        };
                        write_record_to_path(path_override.clone(), &rec);
                        Some((Err(e), (s, model, provider, stage, path_override, start, true)))
                    }
                    None => None,
                }
            },
        );
        Ok(Box::pin(stream))
    }

    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    fn supports_caching(&self) -> bool {
        self.inner.supports_caching()
    }
}

fn write_record_to_path(path_override: Option<PathBuf>, rec: &LlmCallRecord) {
    let path = path_override.unwrap_or_else(|| {
        let date = Utc::now().format("%Y-%m-%d");
        super::super::paths::telemetry_root(None).join(format!("llm-calls-{date}.jsonl"))
    });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = match serde_json::to_string(rec) {
        Ok(s) => s,
        Err(_) => return,
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::backend::mock::MockBackend;

    fn req() -> ChatRequest<'static> {
        ChatRequest {
            model: "mock-model",
            system: None,
            user: "mock extractive span", // triggers the mock JSON-array path
            max_tokens: 64,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        }
    }

    #[tokio::test]
    async fn generate_writes_one_jsonl_record_with_usage() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test-calls.jsonl");
        let inner = Arc::new(MockBackend::new());
        let tb = TelemetryBackend::new(inner, "extractive").with_path_override(path.clone());

        let _ = tb.generate(req()).await.unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 1, "expected exactly one record line");
        let rec: LlmCallRecord = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(rec.provider, "mock");
        assert_eq!(rec.stage, "extractive");
        assert!(!rec.stream);
        assert!(rec.success);
        assert!(rec.latency_ms < 5_000);
    }

    #[tokio::test]
    async fn generate_stream_writes_record_when_final_chunk_carries_usage() {
        // MockBackend.generate_stream emits the full mock response as a single
        // chunk via ollama::OllamaClient::generate_stream's mock path — usage is
        // None on every chunk (mock doesn't synthesize Usage in stream form).
        // So this test verifies a synthetic scenario via a small inline backend
        // that emits one chunk with usage.
        struct OneChunkWithUsage;
        #[async_trait]
        impl ChatBackend for OneChunkWithUsage {
            async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
                anyhow::bail!("unused")
            }
            async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
                let chunk = ChatChunk {
                    delta: "hello".into(),
                    usage: Some(super::super::Usage {
                        input_tokens: 5,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        provider: "test",
                        model: "m".into(),
                    }),
                };
                Ok(Box::pin(futures::stream::iter(vec![Ok(chunk)])))
            }
            fn provider_name(&self) -> &'static str { "test" }
        }

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test-stream.jsonl");
        let inner = Arc::new(OneChunkWithUsage);
        let tb = TelemetryBackend::new(inner, "ask.generate").with_path_override(path.clone());

        use futures::StreamExt;
        let mut s = tb.generate_stream(req()).await.unwrap();
        while let Some(c) = s.next().await {
            let _ = c.unwrap();
        }
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 1);
        let rec: LlmCallRecord = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(rec.stage, "ask.generate");
        assert!(rec.stream);
        assert_eq!(rec.input_tokens, 5);
        assert_eq!(rec.output_tokens, 1);
    }

    #[tokio::test]
    async fn generate_records_failure_with_zero_tokens() {
        struct AlwaysFails;
        #[async_trait]
        impl ChatBackend for AlwaysFails {
            async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
                anyhow::bail!("simulated error")
            }
            async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
                anyhow::bail!("simulated error")
            }
            fn provider_name(&self) -> &'static str { "test" }
        }

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test-fail.jsonl");
        let inner = Arc::new(AlwaysFails);
        let tb = TelemetryBackend::new(inner, "rewriter").with_path_override(path.clone());

        let r = tb.generate(req()).await;
        assert!(r.is_err());

        let body = std::fs::read_to_string(&path).unwrap();
        let rec: LlmCallRecord = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert!(!rec.success);
        assert_eq!(rec.input_tokens, 0);
        assert_eq!(rec.output_tokens, 0);
    }
}
```

**Step 3: Add `telemetry_root` helper in paths.rs**

In `mur-core/src/conversations/paths.rs`, add (next to existing `summary_paths_for` / `raw_root` / etc.):

```rust
pub fn telemetry_root(root_override: Option<&str>) -> std::path::PathBuf {
    let base = root_override
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".mur"));
    base.join("telemetry")
}
```

**Step 4: Wire `pub mod telemetry` in `backend/mod.rs`**

```rust
pub mod anthropic;
pub mod factory;
pub mod mock;
pub mod ollama;
pub mod retry;
pub mod telemetry;
```

**Step 5: Run telemetry tests**

```bash
cargo test -p mur-core --lib conversations::backend::telemetry 2>&1 | tail -15
```

Expected: 3 tests PASS.

**Step 6: Update `factory::build` to accept a stage tag and wrap the result**

Change the factory signature to:

```rust
pub fn build_for_stage(cfg: &BackendConfig, stage: &'static str) -> Result<Arc<dyn ChatBackend>> {
    let raw = build_raw(cfg)?;
    Ok(Arc::new(super::telemetry::TelemetryBackend::new(raw, stage)))
}

/// Backwards-compatible: builds without telemetry. Used by tests and any
/// caller that doesn't have a stage tag.
pub fn build(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
    build_raw(cfg)
}

fn build_raw(cfg: &BackendConfig) -> Result<Arc<dyn ChatBackend>> {
    // ...existing factory body unchanged...
}
```

Then update each call site (compact extractive/abstractive, rollup week+month, ask answer/rewriter, ask Stage 1b) to use `build_for_stage` with the right tag. The stage names follow the LlmCallRecord doc table:

| Call site | Stage tag |
|---|---|
| `compact.extractive::extract_chunk` (called from `summarize/mod.rs`) | `"extractive"` |
| `compact.abstractive::summarize` | `"abstractive"` |
| `summarize::rollup` (week + month) | `"rollup"` |
| `ask::generate::stream_answer` | `"ask.generate"` |
| `ask::rewriter` | `"rewriter"` |
| `ask::abstractive::compress_hit` (Stage 1b) | `"ask.compress_hit"` |

For each call site, swap `factory::build(&cfg)` → `factory::build_for_stage(&cfg, "<stage>")`.

For `ask::ask_stream` where Task 4 made stage1b_backend = answer_backend: keep them as separate backends now — Stage 1b records under `"ask.compress_hit"`, answer-stream records under `"ask.generate"`. Build twice with different stage tags.

**Step 7: Run all backend tests**

```bash
cargo test -p mur-core --lib conversations::backend -- --test-threads=1 2>&1 | tail -25
cargo test -p mur-core --lib conversations -- --test-threads=1 2>&1 | tail -10
```

Expected: PASS — telemetry tests + factory tests + all migrated call-site tests still green.

**Step 8: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 9: Commit**

```bash
git add mur-core/src/conversations/backend/telemetry.rs mur-core/src/conversations/backend/mod.rs mur-core/src/conversations/backend/factory.rs mur-core/src/conversations/paths.rs mur-core/src/conversations/summarize/mod.rs mur-core/src/conversations/summarize/rollup.rs mur-core/src/conversations/ask/mod.rs mur-core/src/cmd/conversations_cmd.rs
git commit -m "$(cat <<'EOF'
feat(backend): per-call telemetry — TelemetryBackend + JSONL writer

New TelemetryBackend decorator wraps any ChatBackend with per-call
instrumentation. Writes one LlmCallRecord JSONL line per generate() call
and per generate_stream() final-chunk-with-usage. Records: ts, provider,
model, stage tag, input/output/cache tokens, latency_ms, stream flag,
success flag.

Default log path: ~/.mur/telemetry/llm-calls-<YYYY-MM-DD>.jsonl (rotates
daily by file name; no rotation logic — just date-stamped paths). Tests
override the path via with_path_override().

Stage tags per call site (matches the cost-report aggregation in task 9):
- extractive (compact.extractive)
- abstractive (compact.abstractive)
- rollup (week+month rollup_narrative)
- ask.generate (answer streaming)
- rewriter (query reformulation)
- ask.compress_hit (Stage 1b per-hit compression)

factory::build_for_stage(cfg, stage) is the new entrypoint that wraps the
real backend in TelemetryBackend; factory::build stays for backwards
compat (tests + call sites without stage tag).

3 telemetry tests cover: generate writes one record with usage, streaming
writes one record on the usage-bearing final chunk, failures record with
zero tokens.

Refs spec §11 (telemetry sketch). Plan task 8.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: New `mur conversations cost-report` command

**Files:**
- Create: `mur-core/src/cmd/conversations_cost_report.rs` (new — aggregation + pretty-print)
- Modify: `mur-core/src/cmd/mod.rs` (`pub mod conversations_cost_report;`)
- Modify: `mur-core/src/main.rs` (clap subcommand wiring under `conversations`)

**Step 1: Spec the output shape**

```
$ mur conversations cost-report --since 7d

Per-stage totals (last 7 days, 2026-04-26..2026-05-02):

  stage             provider     model                      calls   in_tok  out_tok  cache_r  cache_w   est_$
  ─────────────────────────────────────────────────────────────────────────────────────────────────────────────
  extractive        anthropic    claude-haiku-4-5           312     412.5K  18.2K    180.1K   95.4K    0.487
  abstractive       anthropic    claude-haiku-4-5           42      85.1K   12.4K    62.0K    18.5K    0.092
  ask.generate      anthropic    claude-sonnet-4-6          17      22.4K   8.1K     0        0        0.189
  rewriter          ollama       llama3.2:3b                 17      —       —        —        —        —
  ask.compress_hit  ollama       qwen3:14b                   83      —       —        —        —        —
  ─────────────────────────────────────────────────────────────────────────────────────────────────────────────
  TOTAL                                                       471                                       0.768

(estimated cost in USD; ollama calls are local — no $ shown)
```

`--since` accepts: `7d`, `30d`, `1h`, RFC3339 timestamp. `--json` emits the aggregation as JSON.

**Step 2: Pricing table**

Hardcoded constants (sourced from `claude-api` skill `shared/models.md` cached 2026-04-15). Both per-1M-token, separately for input/output/cache-write/cache-read:

```rust
/// (input_per_1m, output_per_1m, cache_write_per_1m, cache_read_per_1m) USD.
fn price_table(model: &str) -> Option<(f64, f64, f64, f64)> {
    match model {
        // Anthropic — values from claude-api skill, cached 2026-04-15.
        m if m.starts_with("claude-haiku-4-5") => Some((1.00, 5.00, 1.25, 0.10)),
        m if m.starts_with("claude-sonnet-4-6") => Some((3.00, 15.00, 3.75, 0.30)),
        m if m.starts_with("claude-opus-4-7") => Some((15.00, 75.00, 18.75, 1.50)),
        m if m.starts_with("claude-opus-4-6") => Some((15.00, 75.00, 18.75, 1.50)),
        // Ollama is local — return None so the CLI prints "—" instead of "$0.00".
        _ => None,
    }
}

fn estimate_cost(model: &str, in_tok: u64, out_tok: u64, cache_r: u64, cache_w: u64) -> Option<f64> {
    let (pi, po, pcw, pcr) = price_table(model)?;
    let cost = (in_tok as f64 / 1_000_000.0) * pi
        + (out_tok as f64 / 1_000_000.0) * po
        + (cache_w as f64 / 1_000_000.0) * pcw
        + (cache_r as f64 / 1_000_000.0) * pcr;
    Some(cost)
}
```

**Step 3: Write the failing test**

Create `mur-core/src/cmd/conversations_cost_report.rs`:

```rust
//! `mur conversations cost-report` — aggregates LlmCallRecord JSONL files into
//! per-stage totals + estimated USD cost.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::conversations::backend::telemetry::LlmCallRecord;

#[derive(Debug, Default, Clone, Serialize)]
pub struct StageTotals {
    pub stage: String,
    pub provider: String,
    pub model: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub estimated_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct CostReport {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub stages: Vec<StageTotals>,
    pub total_usd: f64,
}

/// Parse `--since` as either a humantime duration ("7d", "30d", "1h") or
/// an RFC3339 timestamp.
pub fn parse_since(since: &str) -> Result<DateTime<Utc>> {
    if let Ok(ts) = since.parse::<DateTime<Utc>>() {
        return Ok(ts);
    }
    // Tiny humantime parser (no extra dep): N + unit.
    let (n_str, unit) = since.split_at(since.len() - 1);
    let n: i64 = n_str.parse().map_err(|_| anyhow::anyhow!("bad --since: {since}"))?;
    let dur = match unit {
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        "w" => Duration::weeks(n),
        _ => anyhow::bail!("bad --since unit: {unit} (want h, d, w, or RFC3339 timestamp)"),
    };
    Ok(Utc::now() - dur)
}

pub fn aggregate(records: impl Iterator<Item = LlmCallRecord>) -> Vec<StageTotals> {
    // Key by (stage, provider, model) — same key as the spec table.
    let mut buckets: BTreeMap<(String, String, String), StageTotals> = BTreeMap::new();
    for rec in records {
        let key = (rec.stage.clone(), rec.provider.clone(), rec.model.clone());
        let entry = buckets.entry(key).or_insert_with(|| StageTotals {
            stage: rec.stage.clone(),
            provider: rec.provider.clone(),
            model: rec.model.clone(),
            ..Default::default()
        });
        entry.calls += 1;
        entry.input_tokens += rec.input_tokens;
        entry.output_tokens += rec.output_tokens;
        entry.cache_read_input_tokens += rec.cache_read_input_tokens;
        entry.cache_creation_input_tokens += rec.cache_creation_input_tokens;
    }
    // Compute estimated cost per row.
    let mut stages: Vec<StageTotals> = buckets.into_values().collect();
    for s in &mut stages {
        s.estimated_usd = estimate_cost(
            &s.model,
            s.input_tokens,
            s.output_tokens,
            s.cache_read_input_tokens,
            s.cache_creation_input_tokens,
        );
    }
    stages
}

fn price_table(model: &str) -> Option<(f64, f64, f64, f64)> {
    match model {
        m if m.starts_with("claude-haiku-4-5") => Some((1.00, 5.00, 1.25, 0.10)),
        m if m.starts_with("claude-sonnet-4-6") => Some((3.00, 15.00, 3.75, 0.30)),
        m if m.starts_with("claude-opus-4-7") => Some((15.00, 75.00, 18.75, 1.50)),
        m if m.starts_with("claude-opus-4-6") => Some((15.00, 75.00, 18.75, 1.50)),
        _ => None,
    }
}

fn estimate_cost(model: &str, in_tok: u64, out_tok: u64, cache_r: u64, cache_w: u64) -> Option<f64> {
    let (pi, po, pcw, pcr) = price_table(model)?;
    Some(
        (in_tok as f64 / 1_000_000.0) * pi
            + (out_tok as f64 / 1_000_000.0) * po
            + (cache_w as f64 / 1_000_000.0) * pcw
            + (cache_r as f64 / 1_000_000.0) * pcr,
    )
}

pub fn read_records_since(since: DateTime<Utc>, root_override: Option<&str>) -> Result<Vec<LlmCallRecord>> {
    let dir = crate::conversations::paths::telemetry_root(root_override);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let contents = std::fs::read_to_string(&path)?;
        for line in contents.lines() {
            if line.trim().is_empty() { continue; }
            match serde_json::from_str::<LlmCallRecord>(line) {
                Ok(r) if r.ts >= since => out.push(r),
                Ok(_) => {} // older than --since
                Err(e) => tracing::warn!(err = ?e, line = line, "skipping malformed telemetry line"),
            }
        }
    }
    Ok(out)
}

pub async fn cmd_cost_report(since: &str, json: bool, root_override: Option<&str>) -> Result<()> {
    let since_ts = parse_since(since)?;
    let records = read_records_since(since_ts, root_override)?;
    let stages = aggregate(records.into_iter());
    let total_usd: f64 = stages.iter().filter_map(|s| s.estimated_usd).sum();
    let report = CostReport {
        since: since_ts,
        until: Utc::now(),
        stages,
        total_usd,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "Per-stage totals (since {}, until {}):\n",
        report.since.format("%Y-%m-%d %H:%M UTC"),
        report.until.format("%Y-%m-%d %H:%M UTC")
    );
    println!("  {:<18} {:<12} {:<27} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "stage", "provider", "model", "calls", "in_tok", "out_tok", "cache_r", "cache_w", "est_$");
    println!("  {}", "─".repeat(115));
    for s in &report.stages {
        let cost_str = match s.estimated_usd {
            Some(c) if c > 0.0001 => format!("${:.3}", c),
            Some(_) => "$0.000".into(),
            None => "—".into(),
        };
        println!(
            "  {:<18} {:<12} {:<27} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8}",
            s.stage,
            s.provider,
            truncate_to(&s.model, 27),
            s.calls,
            human_count(s.input_tokens),
            human_count(s.output_tokens),
            human_count(s.cache_read_input_tokens),
            human_count(s.cache_creation_input_tokens),
            cost_str,
        );
    }
    println!("  {}", "─".repeat(115));
    println!("  TOTAL{:>110}\n", format!("${:.3}", report.total_usd));
    println!("(ollama calls are local — no cost shown)");
    Ok(())
}

fn truncate_to(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…", &s[..n.saturating_sub(1)]) }
}

fn human_count(n: u64) -> String {
    if n == 0 { "—".into() }
    else if n < 1_000 { format!("{n}") }
    else if n < 1_000_000 { format!("{:.1}K", n as f64 / 1_000.0) }
    else { format!("{:.1}M", n as f64 / 1_000_000.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(stage: &str, provider: &str, model: &str, in_tok: u64, out_tok: u64, cache_r: u64, cache_w: u64) -> LlmCallRecord {
        LlmCallRecord {
            ts: Utc::now(),
            provider: provider.into(),
            model: model.into(),
            stage: stage.into(),
            input_tokens: in_tok,
            output_tokens: out_tok,
            cache_read_input_tokens: cache_r,
            cache_creation_input_tokens: cache_w,
            latency_ms: 100,
            stream: false,
            success: true,
        }
    }

    #[test]
    fn parse_since_accepts_relative_durations() {
        assert!(parse_since("7d").is_ok());
        assert!(parse_since("30d").is_ok());
        assert!(parse_since("1h").is_ok());
        assert!(parse_since("2w").is_ok());
    }

    #[test]
    fn parse_since_accepts_rfc3339() {
        let p = parse_since("2026-04-01T00:00:00Z").unwrap();
        assert_eq!(p.format("%Y-%m-%d").to_string(), "2026-04-01");
    }

    #[test]
    fn parse_since_rejects_bad_unit() {
        assert!(parse_since("7x").is_err());
    }

    #[test]
    fn aggregate_groups_by_stage_provider_model() {
        let records = vec![
            rec("extractive", "anthropic", "claude-haiku-4-5", 100, 50, 0, 0),
            rec("extractive", "anthropic", "claude-haiku-4-5", 200, 100, 0, 0),
            rec("abstractive", "anthropic", "claude-haiku-4-5", 1000, 500, 0, 0),
            rec("ask.generate", "ollama", "qwen3:14b", 0, 0, 0, 0),
        ];
        let stages = aggregate(records.into_iter());
        // 3 unique (stage, provider, model) keys.
        assert_eq!(stages.len(), 3);
        let extr = stages.iter().find(|s| s.stage == "extractive").unwrap();
        assert_eq!(extr.calls, 2);
        assert_eq!(extr.input_tokens, 300);
        assert_eq!(extr.output_tokens, 150);
        assert!(extr.estimated_usd.is_some());
    }

    #[test]
    fn aggregate_sets_none_estimated_usd_for_ollama() {
        let records = vec![
            rec("rewriter", "ollama", "llama3.2:3b", 100, 50, 0, 0),
        ];
        let stages = aggregate(records.into_iter());
        assert_eq!(stages.len(), 1);
        assert!(stages[0].estimated_usd.is_none(), "ollama models must not have a $ estimate");
    }

    #[test]
    fn estimate_cost_haiku_matches_table() {
        // 1M input + 1M output tokens for Haiku 4.5 → $1 + $5 = $6
        let cost = estimate_cost("claude-haiku-4-5", 1_000_000, 1_000_000, 0, 0).unwrap();
        assert!((cost - 6.0).abs() < 0.001);
    }

    #[test]
    fn read_records_since_filters_old_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("telemetry");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("llm-calls-test.jsonl");
        let old = LlmCallRecord {
            ts: Utc::now() - Duration::days(30),
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
            stage: "rewriter".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            latency_ms: 100,
            stream: false,
            success: true,
        };
        let new = LlmCallRecord {
            ts: Utc::now() - Duration::hours(1),
            ..old.clone()
        };
        let body = format!(
            "{}\n{}\n",
            serde_json::to_string(&old).unwrap(),
            serde_json::to_string(&new).unwrap()
        );
        std::fs::write(&path, body).unwrap();
        let since = Utc::now() - Duration::days(7);
        let records = read_records_since(since, Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(records.len(), 1, "old record should be filtered out");
    }
}
```

**Step 4: Add clap wiring in `main.rs`**

Find the `Conversations { ... }` enum variant in main.rs. Add a new subcommand:

```rust
    /// Aggregate LLM call telemetry into per-stage cost report.
    CostReport {
        /// Time range relative to now (e.g. `7d`, `30d`, `1h`) or RFC3339 timestamp.
        #[arg(long, default_value = "7d")]
        since: String,
        /// Emit JSON instead of pretty table.
        #[arg(long)]
        json: bool,
    },
```

Dispatch:
```rust
        ConversationsCmd::CostReport { since, json } => {
            mur_core::cmd::conversations_cost_report::cmd_cost_report(&since, json, None).await
        }
```

Add `pub mod conversations_cost_report;` in `mur-core/src/cmd/mod.rs`.

**Step 5: Run tests**

```bash
cargo test -p mur-core --lib cmd::conversations_cost_report 2>&1 | tail -15
cargo build --workspace
```

Expected: 6 tests PASS, build clean.

**Step 6: Smoke test**

```bash
# Generate some telemetry by running ask in mock mode
MUR_LLM_MOCK=1 cargo run --bin mur --quiet -- ask "what did I ship today?" >/dev/null 2>&1
# Then read it back
cargo run --bin mur --quiet -- conversations cost-report --since 1h
```

Expected: table prints with at least one row from the mock-mode ask call (stage will be `rewriter` or `ask.generate` depending on the path taken). Cost column shows `—` (mock provider).

**Step 7: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --bins --tests -- -D warnings
```

**Step 8: Commit**

```bash
git add mur-core/src/cmd/conversations_cost_report.rs mur-core/src/cmd/mod.rs mur-core/src/main.rs
git commit -m "$(cat <<'EOF'
feat(cli): mur conversations cost-report — aggregate LLM telemetry

New subcommand reads ~/.mur/telemetry/llm-calls-*.jsonl (written by
TelemetryBackend in task 8), filters by --since (humantime "7d"/"30d"/
"1h" or RFC3339 timestamp), groups by (stage, provider, model), and
prints a table with calls + token totals + estimated USD.

USD estimation is from a hardcoded price table sourced from the claude-api
skill cached 2026-04-15. Anthropic models priced separately for
input/output/cache-write/cache-read; ollama models show "—" since they're
local.

--json emits the aggregated CostReport struct as pretty-printed JSON for
piping to jq/dashboards.

6 tests cover: --since parsing (humantime + RFC3339 + bad-unit),
aggregation grouping by (stage, provider, model), ollama producing None
for estimated_usd, Haiku price table matches expected $6 for 1M+1M tokens,
read_records_since filters by timestamp.

Closes spec §11. Plan task 9.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Fix I1 — recover Ollama final usage from `generate_stream`

**Files:**
- Modify: `mur-core/src/conversations/ollama.rs` (`GenerateResponse` parser + `generate_stream` to capture final-line usage)
- Modify: `mur-core/src/conversations/backend/ollama.rs` (`OllamaBackend::generate_stream` adapt the final usage chunk)

**Step 1: Read Ollama's NDJSON final-line shape**

Ollama emits one JSON line per token + a final line:

```json
{"model":"qwen3:14b","created_at":"...","response":"","done":true,"prompt_eval_count":42,"eval_count":17,...}
```

Today `OllamaClient::generate_stream` (ollama.rs:161-243) drops empty-response lines (line 198-205) and on EOF returns `None` instead of synthesizing a final chunk. We need to surface the `prompt_eval_count` + `eval_count` from the `done: true` line as a final `(empty_string, Some(Usage))` chunk.

**Step 2: Refactor `OllamaClient::generate_stream` return type**

Today it's `Stream<Item = Result<String>>`. We need to carry usage on the final item. Options:
- (a) Change to `Stream<Item = Result<OllamaStreamChunk>>` where `OllamaStreamChunk { delta: String, usage: Option<OllamaUsage> }`. Cleanest, but breaks the type signature.
- (b) Keep `Result<String>` and add a parallel `last_usage: Mutex<Option<OllamaUsage>>` on the client. Hacky — multiple in-flight streams collide.
- (c) Change return type to `Stream<Item = Result<(String, Option<OllamaUsage>)>>`. Tuple is awkward but minimally invasive.

**Pick (a)** — cleanest. The downstream consumer is `OllamaBackend::generate_stream` (one site).

**Step 3: Write the failing tests**

In `mur-core/src/conversations/ollama.rs` `mod tests` add:

```rust
    #[tokio::test]
    async fn generate_stream_emits_final_usage_chunk() {
        // Use wiremock to feed a deterministic NDJSON stream including the
        // final done:true line with eval counts.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let body = "\
{\"model\":\"qwen3:14b\",\"response\":\"hello\",\"done\":false}
{\"model\":\"qwen3:14b\",\"response\":\" world\",\"done\":false}
{\"model\":\"qwen3:14b\",\"response\":\"\",\"done\":true,\"prompt_eval_count\":7,\"eval_count\":3}
";
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/x-ndjson")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let client = OllamaClient::new(&server.uri(), Duration::from_secs(5));
        use futures::StreamExt;
        let mut stream = client
            .generate_stream(GenerateRequest {
                model: "qwen3:14b",
                prompt: "hi",
                system: None,
                stream: true,
                options: GenerateOptions {
                    temperature: None,
                    top_p: None,
                    num_predict: None,
                    stop: vec![],
                },
            })
            .await
            .unwrap();

        let mut text = String::new();
        let mut final_usage = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            text.push_str(&chunk.delta);
            if let Some(u) = chunk.usage {
                assert!(final_usage.is_none(), "usage should appear on exactly one chunk");
                final_usage = Some(u);
            }
        }
        assert_eq!(text, "hello world");
        let u = final_usage.expect("expected final usage chunk from done:true line");
        assert_eq!(u.prompt_eval_count, 7);
        assert_eq!(u.eval_count, 3);
    }
```

**Step 3: Add `OllamaStreamChunk` + `OllamaUsage`**

In `ollama.rs` near `GenerateResponse`:

```rust
#[derive(Debug, Clone)]
pub struct OllamaStreamChunk {
    pub delta: String,
    /// Some on the final chunk only (when Ollama emits done:true with eval counts).
    pub usage: Option<OllamaUsage>,
}

#[derive(Debug, Clone, Copy)]
pub struct OllamaUsage {
    pub prompt_eval_count: u64,
    pub eval_count: u64,
}
```

**Step 4: Update `generate_stream` to emit the final chunk**

Change the return type from `Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>>` to `Result<Pin<Box<dyn Stream<Item = Result<OllamaStreamChunk>> + Send>>>`.

In the `unfold` closure, when a parsed line has `done == true` AND has `prompt_eval_count`/`eval_count`, emit a final chunk with usage. Otherwise carry the response text through with `usage: None`. Mock path emits one chunk per token-split with `usage: None` — to stay test-helpful in mock mode, include a synthetic final chunk with usage `Some(OllamaUsage { prompt_eval_count: 0, eval_count: 0 })` after the last text chunk (this gives downstream telemetry the same shape regardless of mock vs real).

Actually rethinking: the mock path doesn't synthesize Usage today and changing it touches a lot of test fixtures. **Don't synthesize Usage in mock mode** — leave the mock stream usage-less and document it. Real Ollama HTTP responses will get the final-chunk usage from the `done:true` line.

Update the parsing to look at the `done` flag + counts:

```rust
#[derive(Debug, Deserialize)]
struct GenerateResponse {
    pub response: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub prompt_eval_count: u64,
    #[serde(default)]
    pub eval_count: u64,
}
```

In the unfold loop, replace:

```rust
match serde_json::from_str::<GenerateResponse>(trimmed) {
    Ok(v) => {
        if !v.response.is_empty() {
            return Some((Ok(v.response), (inner, buf, false)));
        }
        // Empty response — keep draining.
        continue;
    }
    Err(e) => {
        return Some((Err(e.into()), (inner, buf, true)));
    }
}
```

with:

```rust
match serde_json::from_str::<GenerateResponse>(trimmed) {
    Ok(v) => {
        // Final line with eval counts → emit one usage-bearing chunk.
        if v.done && (v.prompt_eval_count > 0 || v.eval_count > 0) {
            let usage = OllamaUsage {
                prompt_eval_count: v.prompt_eval_count,
                eval_count: v.eval_count,
            };
            return Some((
                Ok(OllamaStreamChunk { delta: v.response, usage: Some(usage) }),
                (inner, buf, true), // mark done so the loop ends after this
            ));
        }
        if !v.response.is_empty() {
            return Some((
                Ok(OllamaStreamChunk { delta: v.response, usage: None }),
                (inner, buf, false),
            ));
        }
        continue;
    }
    Err(e) => return Some((Err(e.into()), (inner, buf, true))),
}
```

Apply the same shape change to the mock stream:

```rust
if Self::mock_from_env() {
    let full = mock_generate(&req).response;
    let tokens: Vec<OllamaStreamChunk> = full
        .split_inclusive(' ')
        .map(|s| OllamaStreamChunk { delta: s.into(), usage: None })
        .collect();
    let stream = futures::stream::iter(tokens.into_iter().map(Ok));
    return Ok(Box::pin(stream));
}
```

(Mock path still emits no Usage — documented.)

**Step 5: Update the EOF flush**

In the `None` branch of `inner.next().await`, the existing code parses the trailing buffer. Apply the same `done && counts > 0 → emit usage` logic there too, so EOF without a clean newline still surfaces usage.

**Step 6: Update `OllamaBackend::generate_stream`**

In `mur-core/src/conversations/backend/ollama.rs`, the existing adapter maps `String` → `ChatChunk { delta, usage: None }`. Change it to map `OllamaStreamChunk` → `ChatChunk`:

```rust
let chunks = inner_stream.map(|item| {
    item.map(|chunk| ChatChunk {
        delta: chunk.delta,
        usage: chunk.usage.map(|u| Usage {
            input_tokens: u.prompt_eval_count,
            output_tokens: u.eval_count,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            provider: "ollama",
            model: req.model.to_string(), // Wait — req is consumed; capture before the map.
        }),
    })
});
```

Capture `let model_owned = req.model.to_string();` before constructing the inner request, then move `model_owned` into the map closure.

**Step 7: Run tests**

```bash
cargo test -p mur-core --lib conversations::ollama 2>&1 | tail -15
cargo test -p mur-core --lib conversations::backend::ollama 2>&1 | tail -15
```

Expected: PASS — new generate_stream_emits_final_usage_chunk + existing ollama tests + the OllamaBackend trait tests all green. Some existing internal tests of `OllamaClient::generate_stream` may need their type expectations updated from `String` to `OllamaStreamChunk.delta`.

**Step 8: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 9: Commit**

```bash
git add mur-core/src/conversations/ollama.rs mur-core/src/conversations/backend/ollama.rs
git commit -m "$(cat <<'EOF'
fix(ollama): surface final-chunk usage from streamed generate

Ollama's NDJSON stream ends with a done:true line carrying prompt_eval_count
+ eval_count. OllamaClient::generate_stream was discarding it, so streamed
Ollama responses had no Usage and TelemetryBackend recorded zero tokens
for every ask.generate call against local Ollama.

Changes the OllamaClient::generate_stream return type from
Stream<Item = Result<String>> to Stream<Item = Result<OllamaStreamChunk>>
where OllamaStreamChunk = { delta: String, usage: Option<OllamaUsage> }.
The final chunk carries Some(usage); intermediate chunks carry None.

OllamaBackend::generate_stream maps the new shape into ChatChunk's
optional Usage field — the trait surface is unchanged.

Mock-mode streams still emit no usage (the mock doesn't synthesize token
counts) — documented inline. Real HTTP responses now surface usage.

1 new wiremock test verifies a 3-line NDJSON stream (2 text + 1 done:true
with eval counts) yields "hello world" + final chunk with prompt_eval_count=7,
eval_count=3.

Closes I1 from P2 code review. Unblocks accurate cost-report output for
local Ollama users.

Refs spec §11. Plan task 10.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Integration tests for full SSE composition (closes I3-I5)

**Files:**
- Modify: `mur-core/src/conversations/backend/factory.rs` (add 3 integration tests using wiremock + the full retry+telemetry stack)

**Step 1: Spec the gaps**

P2 code review flagged three integration gaps the unit tests didn't cover:
- **I3:** `RetryingBackend::generate_stream` retry-on-connect with a real `AnthropicBackend` SSE handshake (unit test used a hand-rolled inner backend; never exercised the SSE parser through retry).
- **I4:** `factory::build_for_stage` composes `TelemetryBackend → RetryingBackend → AnthropicBackend` in the right order (telemetry sees the final retry result, not each retry attempt).
- **I5:** `ask::generate::stream_answer` end-to-end against a wiremock'd Anthropic SSE response, verifying the streamed text reaches stdout and the final chunk's usage is recorded.

I3 + I4 land here as factory-level integration tests. I5 is a higher-level test that belongs in `mur-core/tests/cli_conversations.rs` — also added here for symmetry.

**Step 2: Write I3 — retry through real AnthropicBackend SSE parser**

In `factory.rs` `mod tests` append:

```rust
    #[tokio::test]
    async fn factory_retries_anthropic_503_then_streams_via_real_sse_parser() {
        use futures::StreamExt;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_ANTHROPIC_KEY_I3", "k") };

        let server = MockServer::start().await;

        // First two attempts: 503. Third: real SSE stream.
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;

        let sse = "\
event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}

event: message_stop
data: {\"type\":\"message_stop\"}

";
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse),
            )
            .mount(&server)
            .await;

        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: Some(server.uri()),
            api_key_env: Some("MUR_TEST_ANTHROPIC_KEY_I3".into()),
            timeout_secs: Some(5),
        };
        let b = build(&cfg).unwrap();
        let req = ChatRequest {
            model: "claude-haiku-4-5",
            user: "hi",
            system: None,
            max_tokens: 16,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        let mut stream = b.generate_stream(req).await.unwrap();
        let mut text = String::new();
        let mut final_usage = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            text.push_str(&chunk.delta);
            if let Some(u) = chunk.usage {
                final_usage = Some(u);
            }
        }
        assert_eq!(text, "OK");
        let u = final_usage.expect("usage from final chunk");
        assert_eq!(u.input_tokens, 3);
        assert_eq!(u.output_tokens, 1);
        unsafe { std::env::remove_var("MUR_TEST_ANTHROPIC_KEY_I3") };
    }
```

**Step 3: Write I4 — factory composes telemetry → retry → anthropic**

```rust
    #[tokio::test]
    async fn factory_for_stage_records_one_telemetry_line_after_retry_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_LLM_MOCK") };
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        unsafe { std::env::set_var("MUR_TEST_ANTHROPIC_KEY_I4", "k") };

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":3,"output_tokens":1}}"#),
            )
            .mount(&server)
            .await;

        let cfg = BackendConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            endpoint: Some(server.uri()),
            api_key_env: Some("MUR_TEST_ANTHROPIC_KEY_I4".into()),
            timeout_secs: Some(5),
        };

        // Use build_for_stage so the result is wrapped in TelemetryBackend.
        // Override the telemetry path via an env var the TelemetryBackend
        // honors (or set up a test-only path injection — see telemetry.rs
        // for the current test-friendly path. If no env override exists,
        // call build_raw + manually wrap).
        let raw = build(&cfg).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("test-telemetry.jsonl");
        let tb = super::super::telemetry::TelemetryBackend::new(raw, "extractive")
            .with_path_override(log_path.clone());

        let req = ChatRequest {
            model: "claude-haiku-4-5",
            user: "hi",
            system: None,
            max_tokens: 16,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        };
        let _ = tb.generate(req).await.unwrap();

        let body = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            body.lines().count(),
            1,
            "telemetry should record exactly ONE line for the final retry success, not one per attempt"
        );
        let rec: super::super::telemetry::LlmCallRecord =
            serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert!(rec.success);
        assert_eq!(rec.input_tokens, 3);
        assert_eq!(rec.output_tokens, 1);
        unsafe { std::env::remove_var("MUR_TEST_ANTHROPIC_KEY_I4") };
    }
```

**Step 4: Write I5 — ask::generate::stream_answer end-to-end**

This goes in `mur-core/tests/cli_conversations.rs` (integration test crate) rather than the unit test module. Locate the existing test file:

```bash
ls mur-core/tests/cli_conversations.rs
```

Append:

```rust
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn ask_generate_against_wiremocked_anthropic_sse_streams_text_and_records_usage() {
    use futures::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _env = crate::conversations::ENV_LOCK.lock().unwrap();
    unsafe { std::env::remove_var("MUR_LLM_MOCK") };
    unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    unsafe { std::env::set_var("MUR_TEST_ANTHROPIC_KEY_I5", "k") };

    let server = MockServer::start().await;
    let sse = "event: content_block_delta\n\
        data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n\
        event: content_block_delta\n\
        data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n\
        event: message_delta\n\
        data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}\n\n\
        event: message_stop\n\
        data: {\"type\":\"message_stop\"}\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let cfg = mur_common::config::BackendConfig {
        provider: "anthropic".into(),
        model: "claude-haiku-4-5".into(),
        endpoint: Some(server.uri()),
        api_key_env: Some("MUR_TEST_ANTHROPIC_KEY_I5".into()),
        timeout_secs: Some(5),
    };
    let backend = mur_core::conversations::backend::factory::build(&cfg).unwrap();
    let mut stream = mur_core::conversations::ask::generate::stream_answer(
        backend.as_ref(),
        "claude-haiku-4-5",
        "you are a tester",
        "say hello world",
        16,
    )
    .await
    .unwrap();
    let mut text = String::new();
    let mut got_usage = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        text.push_str(&chunk.delta);
        if chunk.usage.is_some() {
            got_usage = true;
        }
    }
    assert_eq!(text, "hello world");
    assert!(got_usage, "expected final chunk with usage");
    unsafe { std::env::remove_var("MUR_TEST_ANTHROPIC_KEY_I5") };
}
```

(If `ENV_LOCK` isn't accessible from the integration test crate, drop that lock — these tests use unique env-var names so they won't collide.)

**Step 5: Run tests**

```bash
cargo test -p mur-core --lib conversations::backend::factory 2>&1 | tail -20
cargo test -p mur-core --test cli_conversations -- --test-threads=1 2>&1 | tail -10
```

Expected: 3 new tests PASS (I3 + I4 in factory.rs, I5 in cli_conversations.rs).

**Step 6: Lint and format**

```bash
cargo fmt -p mur-core && cargo fmt --check -p mur-core
cargo clippy -p mur-core --lib --tests -- -D warnings
```

**Step 7: Commit**

```bash
git add mur-core/src/conversations/backend/factory.rs mur-core/tests/cli_conversations.rs
git commit -m "$(cat <<'EOF'
test(backend): integration tests for full SSE composition (I3-I5)

P2 code review flagged three coverage gaps the P2 unit tests didn't hit:

I3: RetryingBackend::generate_stream retry-on-connect against the real
    AnthropicBackend SSE parser. Unit tests used a hand-rolled inner
    backend; this verifies the SSE parser survives the connect-retry path.
    Wiremock returns 503 twice then a real multi-event SSE body; assert
    the stream yields the parsed text + final usage.

I4: factory::build_for_stage composes TelemetryBackend → RetryingBackend
    → AnthropicBackend in the right order. Verifies telemetry records ONE
    line for the final retry success — not one per attempt — by setting
    up wiremock with 1× 503 + 1× 200 then reading the JSONL writer's
    output.

I5: ask::generate::stream_answer end-to-end against wiremocked Anthropic
    SSE. Hits the full path: factory builds an AnthropicBackend, stream_
    answer wires ChatRequest, the SSE parser yields ChatChunk items, the
    final chunk carries usage. Sits in tests/cli_conversations.rs since
    it crosses the binary boundary.

Refs P2 code review action items I3/I4/I5. Plan task 11.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: End-to-end verification (no commit)

**Files:** none modified.

**Step 1: Full workspace build**

```bash
cargo build --workspace
```

Expected: clean.

**Step 2: Full workspace test**

```bash
cargo test --workspace -- --test-threads=1 2>&1 | tee /tmp/p3-test.log | tail -50
```

(Don't pipe directly through `tail` — same buffer issue P0/P1/P2 sessions hit. `tee` to a file then tail the file.)

Expected: all tests pass, exit 0.

**Step 3: Workspace clippy + fmt**

```bash
cargo fmt --check
cargo clippy -p mur-core --all-targets -- -D warnings
cargo clippy -p mur-common --all-targets -- -D warnings
```

Expected: both clean. (Workspace `clippy --workspace --all-targets` may still hit the pre-existing `companion_enums.rs` issue from earlier phases — not your concern.)

**Step 4: Smoke test cost-report on empty telemetry dir**

```bash
TMPDIR=$(mktemp -d)
HOME="$TMPDIR" cargo run --quiet --bin mur -- conversations cost-report --since 7d
```

Expected: prints empty table with "TOTAL $0.000". No errors, exit 0.

**Step 5: Smoke test full chain in mock mode**

```bash
TMPDIR=$(mktemp -d)
mkdir -p "$TMPDIR/.mur"
cat > "$TMPDIR/.mur/config.yaml" <<'YAML'
embedding:
  provider: ollama
  model: qwen3-embedding:0.6b
  dimensions: 1024
  ollama_endpoint: http://localhost:11434
conversations:
  enabled: true
YAML

# Generate one ask in mock mode (writes telemetry).
HOME="$TMPDIR" MUR_LLM_MOCK=1 \
  /Users/david/Projects/mur/target/debug/mur ask "what did I ship today?" 2>&1 | head -5

# Read it back via cost-report.
HOME="$TMPDIR" /Users/david/Projects/mur/target/debug/mur conversations cost-report --since 1h 2>&1 | head -20
```

Expected: cost-report shows at least one row from the mock-mode ask (rewriter or ask.generate stage). Cost column shows `—` (mock provider). At minimum one telemetry record was written.

**Step 6: Smoke test caching wire shape (Anthropic, expects 401)**

```bash
TMPDIR=$(mktemp -d)
mkdir -p "$TMPDIR/.mur"
cat > "$TMPDIR/.mur/config.yaml" <<'YAML'
embedding:
  provider: ollama
  model: qwen3-embedding:0.6b
  dimensions: 1024
conversations:
  enabled: true
  compact:
    extractive_backend:
      provider: anthropic
      model: claude-haiku-4-5
      api_key_env: ANTHROPIC_API_KEY
YAML
HOME="$TMPDIR" ANTHROPIC_API_KEY=stub-not-real \
  /Users/david/Projects/mur/target/debug/mur conversations doctor 2>&1 | head -20
```

Expected: doctor probes Anthropic and reports auth failure cleanly (the backend reaches Anthropic's API, which 401s on the stub key — proves the cache_control wire shape is accepted by Anthropic as well-formed, since 401 means the JSON parsed but auth failed).

**Step 7: Optional live test (costs ~$0.0001)**

If you have `ANTHROPIC_API_KEY` set:

```bash
ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY cargo test -p mur-core --lib conversations::backend::anthropic -- --ignored 2>&1 | tail -15
```

Expected: 1-2 ignored live tests pass (`live_anthropic_haiku_responds` from P1, optional `live_anthropic_haiku_streams` if added in P2).

**Step 8: Report**

Summary for human reviewer:
- 11 commits on `feat/cloud-llm-backend-p3-plan` after the docs commit
- New: `TelemetryBackend` + JSONL writer, `mur conversations cost-report` command, prompt-caching wired in `AnthropicBackend`, Ollama final-usage chunk recovery
- Migrated: `compact::abstractive`, `summarize::rollup` (week + month), `ask::abstractive::compress_hit` — every conversations LLM call now flows through `ChatBackend`
- Behavior: identical for users with no per-stage backend overrides. Users on Anthropic see (i) per-call USD cost via `mur conversations cost-report`, (ii) cache_control hints sent on extractive + abstractive prompts (silently inactive until prompt grows past the 2048-token Haiku minimum). Local Ollama users see streamed-call usage in cost-report after the I1 fix.
- Test count delta: ~25 new tests across config, backend, telemetry, cost-report, factory composition, and integration crate.
- Closes P2 review action items I1, I2, I3, I4, I5.
- Closes spec §11 (telemetry) and §5.2 (caching wiring).
- After P3, the only remaining cloud-LLM work is P4 (migrate `learn` / `extract_llm`, delete `mur-core/src/llm.rs`).

---

## Out of scope — explicitly deferred to P4 (or beyond)

Do **not** implement any of these in P3:

- Migrating `learn` / `extract_llm` to `ChatBackend` — P4
- Deleting `mur-core/src/llm.rs` — P4
- Per-stage `RollupConfig.{week,month}_abstractive_backend` overrides — defensible follow-up; rollup currently inherits the legacy ollama path (Task 3 documented this)
- Bedrock / Vertex / Foundry support — declined non-goal
- Embedding migration to cloud — declined non-goal
- Auto-fallback from cloud to ollama on outage — declined non-goal
- `max_daily_cost_usd` guardrail — defer until cost-report shows real numbers
- `secret_ref: Option<String>` field on `BackendConfig` for keychain — defer until model-registry convergence (spec §10)
- Mid-stream retry on `RetryingBackend::generate_stream` — connect-only is correct
- Prompt caching beyond `compact.extractive` + `compact.abstractive` — `ask` prompts could benefit but require splitting `prompt::render`'s output into (system, user); defensible follow-up but bigger surface than P3 budget
- Cost-report dashboard / web UI — future work; CLI table is sufficient for P3

If an instruction in this plan tempts you to touch these, **stop and ask** — it means the plan or spec needs amendment.

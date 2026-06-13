# mur-compress Auto-Wire Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make MUR's existing `mur-compress` engine fire automatically and size-gated on the two LLM-facing surfaces (MCP tool outputs, agent-runtime `post_tool_use`), with a teaching skill and a measured use-vs-no-use comparison.

**Architecture:** A new config-agnostic facade `mur_compress::auto` (token gate + shared retrieval envelope) is the single keystone. Two thin call sites use it: a `call_tool` wrapper in `mur-mcp-server` (Surface 1, fully effective, query-aware) and a `CompressHook` in `mur-agent-runtime` (Surface 2, mirrors `B0SafetyHook`). On-by-default, double-gated (token threshold + the engine's existing worth-it ratio), reversible via the CCR store.

**Tech Stack:** Rust (workspace crates `mur-compress`, `mur-mcp-server`, `mur-agent-runtime`, `mur-core`), `serde`/`serde_json`/`serde_yaml`, `async-trait`, `tokio`, `tempfile` (dev). Bench harness in Bash driving the `mur compress` CLI + `jq`.

**Spec:** `docs/superpowers/specs/2026-06-14-mur-compress-autowire-design.md`

**Known limitation (Surface 2):** `mur-agent-runtime/src/hooks/b0.rs:571-574` documents that the supervisor "does not yet act on `replace_output`." So `CompressHook` is correct and unit-tested at the hook level, but its end-to-end effect on what agents read is gated on supervisor wiring that lands separately — exactly the same status as B0's redaction (rule 8). Surface 1 (MCP) has no such gap.

---

## Pre-flight

- [ ] **Create a feature branch** (mur is PR-only to `main`):

Run: `cd /Volumes/Firecuda4tb/Projects/mur && git checkout -b feat/compress-autowire`
Expected: `Switched to a new branch 'feat/compress-autowire'`

---

## File Structure

**Unit A — facade (`mur-compress`)**
- Create: `mur-compress/src/auto.rs` — `AutoCfg`, `AutoOutcome`, `auto_compress`, `retrieval_note`, `retrieval_envelope` + tests
- Modify: `mur-compress/src/config.rs` — add `pub auto: AutoCfg` field + default
- Modify: `mur-compress/src/lib.rs` — `pub mod auto;`, re-exports, `count_tokens()` + `config()` accessors

**Unit B — Surface 1 (`mur-mcp-server`)**
- Modify: `mur-mcp-server/src/tools.rs` — rename match to `dispatch_tool`, new `call_tool` wrapper, `apply_auto_compress` + `maybe_compress_tool_output`, inline tests
- Modify: `mur-mcp-server/Cargo.toml` — ensure `tempfile` dev-dependency

**Unit C — Surface 2 (`mur-agent-runtime`)**
- Create: `mur-agent-runtime/src/hooks/compress.rs` — `CompressHook`
- Modify: `mur-agent-runtime/src/hooks/mod.rs` — `pub mod compress;` + `pub use compress::CompressHook;`
- Modify: `mur-agent-runtime/src/hooks/builder.rs` — register `CompressHook` gated on `auto`
- Modify: `mur-agent-runtime/Cargo.toml` — add `mur-compress` dependency
- Create: `mur-agent-runtime/tests/compress_hook.rs` — hook tests

**Unit D — skill + README**
- Create: `mur-core/src/skills/mur_compress.yaml` — teaching skill
- Modify: `mur-core/src/cmd/sync_cmd.rs` — register skill in `ensure_mur_skill`
- Create: `mur-compress/README.md` — reference + headroom credit

**Unit E — bench harness**
- Create: `scripts/compress-bench.sh` — corpus gen + `mur compress` + parse + `results.json`

**Unit F — deliverables**
- Create: `docs/mur-compress-autowire-report.md` — comparison table + recap
- Create: `docs/mur-compress-autowire.svg` — architecture + savings diagram

---

## Task A: `mur_compress::auto` facade (keystone)

**Files:**
- Create: `mur-compress/src/auto.rs`
- Modify: `mur-compress/src/config.rs:23-35,57-73`
- Modify: `mur-compress/src/lib.rs:4-21,39-56`

- [ ] **Step 1: Add accessors to `CompressEngine`**

In `mur-compress/src/lib.rs`, inside `impl CompressEngine` (after `pub fn new(...)` ends, ~line 56), add:

```rust
    /// Token count for `content` using the engine's configured tokenizer.
    pub fn count_tokens(&self, content: &str) -> usize {
        self.tok.count(content)
    }

    /// Read-only access to the engine's configuration (e.g. the `auto` gates).
    pub fn config(&self) -> &CompressConfig {
        &self.config
    }
```

- [ ] **Step 2: Declare the module and re-exports in `lib.rs`**

In `mur-compress/src/lib.rs`, add `pub mod auto;` to the module list (after `pub mod bm25;`, ~line 4) and add this re-export after the existing `pub use` block (~line 21):

```rust
pub use auto::{AutoCfg, AutoOutcome, auto_compress, retrieval_envelope, retrieval_note};
```

- [ ] **Step 3: Add `auto` to `CompressConfig`**

In `mur-compress/src/config.rs`: add the import at the top (after line 1):

```rust
use crate::auto::AutoCfg;
```

Add the field to the `CompressConfig` struct (after `pub stats: StatsCfg,`, line 34):

```rust
    #[serde(default)]
    pub auto: AutoCfg,
```

Add to the `Default for CompressConfig` impl (after `stats: StatsCfg::default(),`, line 70):

```rust
            auto: AutoCfg::default(),
```

- [ ] **Step 4: Write the failing test (create `auto.rs` with tests, empty impls)**

Create `mur-compress/src/auto.rs`:

```rust
//! Auto-compression facade: a size-gated wrapper over `CompressEngine::compress`
//! plus a shared retrieval envelope, used by every LLM-facing call site
//! (MCP tool outputs, agent-runtime `post_tool_use`). Config-agnostic: callers
//! pass `min_tokens` and check the `auto.*` flags themselves.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::CompressEngine;

/// `auto:` section of `compress.yaml`. Controls *automatic* compression at
/// LLM-facing call sites. The manual `mur_compress`/`mur_retrieve` tools are
/// unaffected by these flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCfg {
    /// Master switch for all automatic compression.
    pub enabled: bool,
    /// Outputs counting fewer than this many tokens are never auto-compressed.
    pub min_tokens: usize,
    /// Surface 1: compress MCP tool outputs.
    pub mcp: bool,
    /// Surface 2: compress agent-runtime `post_tool_use` outputs.
    pub agent_runtime: bool,
}

impl Default for AutoCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            min_tokens: 1500,
            mcp: true,
            agent_runtime: true,
        }
    }
}

/// Outcome of an [`auto_compress`] call.
#[derive(Debug, Clone)]
pub struct AutoOutcome {
    /// Compressed text if `fired`, else the original text unchanged.
    pub text: String,
    /// Present only when content was offloaded to the CCR store.
    pub hash: Option<String>,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    /// True iff the original was replaced with something strictly smaller.
    pub fired: bool,
}

/// Size-gated compression. Never errors: any failure or non-payoff returns the
/// original text with `fired: false`. `min_tokens` is the caller's gate; the
/// engine's own `bloat_threshold` is the second gate.
pub fn auto_compress(
    engine: &CompressEngine,
    text: &str,
    query: Option<&str>,
    min_tokens: usize,
) -> AutoOutcome {
    let before = engine.count_tokens(text);
    if before < min_tokens {
        return AutoOutcome {
            text: text.to_string(),
            hash: None,
            original_tokens: before,
            compressed_tokens: before,
            fired: false,
        };
    }
    let r = engine.compress(text, query);
    // compress() returns a passthrough (tokens_saved == 0) when it doesn't pay off.
    let fired = r.tokens_saved > 0;
    AutoOutcome {
        text: r.compressed,
        hash: r.hash,
        original_tokens: r.original_tokens,
        compressed_tokens: r.compressed_tokens,
        fired,
    }
}

/// Model-readable hint describing how to recover the full content.
pub fn retrieval_note(hash: Option<&str>, query: Option<&str>) -> String {
    match hash {
        Some(h) => match query {
            Some(q) => format!(
                "Large output compressed; original stored. Call mur_retrieve with hash=\"{h}\" (optionally query=\"{q}\") for the full result."
            ),
            None => format!(
                "Large output compressed; original stored. Call mur_retrieve with hash=\"{h}\" for the full result."
            ),
        },
        None => "Output densified in place; nothing offloaded.".to_string(),
    }
}

/// Standard envelope wrapping an offloaded (hash-bearing) compressed result.
/// Both surfaces use this so the model always sees one shape.
pub fn retrieval_envelope(outcome: &AutoOutcome, query: Option<&str>) -> Value {
    json!({
        "compressed": true,
        "content": outcome.text,
        "hash": outcome.hash,
        "original_tokens": outcome.original_tokens,
        "compressed_tokens": outcome.compressed_tokens,
        "note": retrieval_note(outcome.hash.as_deref(), query),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompressConfig, CompressEngine};

    fn engine() -> CompressEngine {
        let dir = tempfile::tempdir().unwrap();
        CompressEngine::new(dir.path().to_path_buf(), CompressConfig::default()).unwrap()
    }

    fn big_json_array() -> String {
        let items: Vec<String> = (0..4000)
            .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\",\"value\":{}}}", i * 7))
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn small_input_is_gated_out() {
        let eng = engine();
        let out = auto_compress(&eng, "tiny output", None, 1500);
        assert!(!out.fired);
        assert_eq!(out.text, "tiny output");
        assert!(out.hash.is_none());
    }

    #[test]
    fn large_json_array_fires_and_offloads() {
        let eng = engine();
        let out = auto_compress(&eng, &big_json_array(), None, 100);
        assert!(out.fired, "large json array should compress");
        assert!(out.hash.is_some(), "json array offload should produce a hash");
        assert!(out.compressed_tokens < out.original_tokens);
    }

    #[test]
    fn gate_uses_min_tokens() {
        let eng = engine();
        // Same large input, but an enormous gate ⇒ nothing fires.
        let out = auto_compress(&eng, &big_json_array(), None, 1_000_000);
        assert!(!out.fired);
    }

    #[test]
    fn envelope_has_stable_shape() {
        let eng = engine();
        let out = auto_compress(&eng, &big_json_array(), Some("item"), 100);
        let env = retrieval_envelope(&out, Some("item"));
        assert_eq!(env["compressed"], serde_json::json!(true));
        assert!(env["hash"].as_str().is_some());
        assert!(env["note"].as_str().unwrap().contains("mur_retrieve"));
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo test -p mur-compress auto`
Expected: PASS (4 new tests in `auto::tests` + existing config tests). If `tempfile` is missing from `[dev-dependencies]` of `mur-compress`, add `tempfile = { workspace = true }` and re-run (the existing `config.rs` tests already use `tempfile::tempdir`, so it should be present).

- [ ] **Step 6: Format, clippy, commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo fmt -p mur-compress
cargo clippy -p mur-compress -- -D warnings
git add mur-compress/src/auto.rs mur-compress/src/config.rs mur-compress/src/lib.rs
git commit -m "feat(compress): add auto-compression facade (AutoCfg, auto_compress, envelope)"
```

---

## Task B: Surface 1 — MCP output path

**Files:**
- Modify: `mur-mcp-server/src/tools.rs:7,343` (+ new helpers and tests at end of file)
- Modify: `mur-mcp-server/Cargo.toml` (`[dev-dependencies]`)

- [ ] **Step 1: Add `AutoCfg` to the mur-compress import**

In `mur-mcp-server/src/tools.rs:7`, change:

```rust
use mur_compress::{CompressConfig, CompressEngine, RetrieveResult};
```
to:
```rust
use mur_compress::{AutoCfg, CompressConfig, CompressEngine, RetrieveResult};
```

- [ ] **Step 2: Rename the dispatch match**

In `mur-mcp-server/src/tools.rs:343`, change the signature line:

```rust
pub async fn call_tool(name: &str, arguments: &Value) -> Result<Value, String> {
```
to:
```rust
async fn dispatch_tool(name: &str, arguments: &Value) -> Result<Value, String> {
```
Leave the entire `match name { … }` body unchanged. (Pure rename — the wrapper added in Step 3 restores the public `call_tool`.)

- [ ] **Step 3: Add the public wrapper + helpers**

Insert immediately **above** `async fn dispatch_tool` (i.e. just before line 343) in `mur-mcp-server/src/tools.rs`:

```rust
/// Tool names whose outputs must never be auto-compressed.
const AUTO_COMPRESS_SKIP: &[&str] = &["mur_compress", "mur_retrieve", "mur_compress_stats"];

/// Public entry point: dispatch the tool, then size-gate auto-compress the
/// result (Surface 1). The compression boundary is exactly the text the model
/// reads from MUR tools.
pub async fn call_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    let out = dispatch_tool(name, arguments).await?;
    Ok(maybe_compress_tool_output(name, arguments, out))
}

/// Apply size-gated auto-compression to a tool result. Unit-testable: takes an
/// explicit engine + auto config, touches no env/filesystem beyond the engine.
fn apply_auto_compress(
    engine: &CompressEngine,
    auto: &AutoCfg,
    name: &str,
    arguments: &Value,
    out: Value,
) -> Value {
    if !auto.enabled || !auto.mcp || AUTO_COMPRESS_SKIP.contains(&name) {
        return out;
    }
    // Pulling args["query"] makes search-style tools query-aware (BM25-retrievable)
    // with no per-handler edits.
    let query = arguments.get("query").and_then(|v| v.as_str());
    let text = match &out {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let outcome = mur_compress::auto_compress(engine, &text, query, auto.min_tokens);
    if !outcome.fired {
        return out;
    }
    match outcome.hash {
        Some(_) => mur_compress::retrieval_envelope(&outcome, query),
        None => Value::String(outcome.text),
    }
}

/// Build the per-call engine and apply auto-compression. Falls back to the
/// uncompressed output if the engine can't be built.
fn maybe_compress_tool_output(name: &str, arguments: &Value, out: Value) -> Value {
    let engine = match compress_engine() {
        Ok(e) => e,
        Err(_) => return out,
    };
    let auto = engine.config().auto.clone();
    apply_auto_compress(&engine, &auto, name, arguments, out)
}
```

- [ ] **Step 4: Append inline tests at the end of `tools.rs`**

```rust
#[cfg(test)]
mod auto_compress_tests {
    use super::*;
    use mur_compress::{CompressConfig, CompressEngine};
    use serde_json::json;

    fn engine() -> CompressEngine {
        let dir = tempfile::tempdir().unwrap();
        CompressEngine::new(dir.path().to_path_buf(), CompressConfig::default()).unwrap()
    }

    fn big_results() -> Value {
        let results: Vec<Value> = (0..3000)
            .map(|i| json!({"file": format!("src/f{i}.rs"), "score": 0.5, "content": format!("fn item_{i}() {{}}")}))
            .collect();
        json!({"results": results, "count": 3000})
    }

    #[test]
    fn skips_compression_tools() {
        let eng = engine();
        let auto = AutoCfg { enabled: true, min_tokens: 1, mcp: true, agent_runtime: true };
        let big = json!({"content": "x".repeat(100_000)});
        let out = apply_auto_compress(&eng, &auto, "mur_compress", &json!({}), big.clone());
        assert_eq!(out, big, "compression tools must pass through");
    }

    #[test]
    fn small_output_unchanged() {
        let eng = engine();
        let auto = AutoCfg::default(); // min_tokens 1500
        let small = json!({"results": ["a", "b"], "count": 2});
        let out = apply_auto_compress(&eng, &auto, "mur_project_search", &json!({"query": "x"}), small.clone());
        assert_eq!(out, small);
    }

    #[test]
    fn large_search_output_compressed_with_query() {
        let eng = engine();
        let auto = AutoCfg { enabled: true, min_tokens: 50, mcp: true, agent_runtime: true };
        let out = apply_auto_compress(&eng, &auto, "mur_project_search", &json!({"query": "item"}), big_results());
        assert_eq!(out.get("compressed").and_then(|v| v.as_bool()), Some(true));
        assert!(out.get("hash").and_then(|v| v.as_str()).is_some());
        assert!(out.get("note").and_then(|v| v.as_str()).unwrap().contains("mur_retrieve"));
    }

    #[test]
    fn disabled_auto_passes_through() {
        let eng = engine();
        let auto = AutoCfg { enabled: false, ..AutoCfg::default() };
        let big = big_results();
        let out = apply_auto_compress(&eng, &auto, "mur_project_search", &json!({}), big.clone());
        assert_eq!(out, big);
    }
}
```

- [ ] **Step 5: Ensure `tempfile` dev-dependency**

Check `mur-mcp-server/Cargo.toml` for a `[dev-dependencies]` section containing `tempfile`. If absent, add:

```toml
[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 6: Run tests**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo test -p mur-mcp-server auto_compress`
Expected: PASS (4 tests). The dispatch match is unchanged, so existing tool tests stay green.

- [ ] **Step 7: Format, clippy, commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo fmt -p mur-mcp-server
cargo clippy -p mur-mcp-server -- -D warnings
git add mur-mcp-server/src/tools.rs mur-mcp-server/Cargo.toml
git commit -m "feat(mcp): auto-compress large tool outputs at call_tool boundary (Surface 1)"
```

---

## Task C: Surface 2 — agent-runtime `CompressHook`

**Files:**
- Create: `mur-agent-runtime/src/hooks/compress.rs`
- Modify: `mur-agent-runtime/src/hooks/mod.rs:18,26`
- Modify: `mur-agent-runtime/src/hooks/builder.rs:25-31`
- Modify: `mur-agent-runtime/Cargo.toml:30`
- Create: `mur-agent-runtime/tests/compress_hook.rs`

- [ ] **Step 1: Add the `mur-compress` dependency**

In `mur-agent-runtime/Cargo.toml`, under `[dependencies]` (after line 30 `mur-common = { path = "../mur-common" }`), add:

```toml
mur-compress = { path = "../mur-compress" }
```

- [ ] **Step 2: Write the failing test**

Create `mur-agent-runtime/tests/compress_hook.rs`:

```rust
//! Surface 2: `CompressHook::post_tool_use` auto-compresses oversized tool
//! outputs into a retrieval envelope. Targets the hook directly (not the chain),
//! mirroring `tests/b0_rule8_memory_redaction.rs`.

use mur_agent_runtime::hooks::{CompressHook, Hook, HookCtx, ToolCall, ToolResult};
use mur_compress::CompressConfig;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn cfg_with_min(min_tokens: usize) -> CompressConfig {
    let mut c = CompressConfig::default();
    c.auto.min_tokens = min_tokens;
    c
}

fn big_output() -> serde_json::Value {
    let results: Vec<_> = (0..3000)
        .map(|i| json!({"file": format!("f{i}.rs"), "score": 0.5}))
        .collect();
    json!({"results": results})
}

#[tokio::test]
async fn large_output_is_compressed_into_envelope() {
    let dir = TempDir::new().unwrap();
    let hook = CompressHook::new(dir.path().join("compress"), cfg_with_min(50));
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("project.search", json!({"query": "item"}));
    let result = ToolResult {
        call_id: "test-call".into(),
        ok: true,
        output: big_output(),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook.post_tool_use(&ctx, &call, &result, &cancel).await.unwrap();
    let replaced = patch.replace_output.expect("compression patch expected");
    assert_eq!(replaced.get("compressed").and_then(|v| v.as_bool()), Some(true));
    assert!(replaced.get("hash").and_then(|v| v.as_str()).is_some());
}

#[tokio::test]
async fn small_output_passes_through() {
    let dir = TempDir::new().unwrap();
    let hook = CompressHook::new(dir.path().join("compress"), cfg_with_min(1500));
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("project.search", json!({}));
    let result = ToolResult {
        call_id: "test-call".into(),
        ok: true,
        output: json!({"results": ["a", "b"]}),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook.post_tool_use(&ctx, &call, &result, &cancel).await.unwrap();
    assert!(patch.replace_output.is_none(), "small output must not be compressed");
}

#[tokio::test]
async fn disabled_when_agent_runtime_flag_off() {
    let dir = TempDir::new().unwrap();
    let mut cfg = cfg_with_min(50);
    cfg.auto.agent_runtime = false;
    let hook = CompressHook::new(dir.path().join("compress"), cfg);
    let ctx = HookCtx::for_test_with_home(dir.path().to_path_buf(), 1);
    let call = ToolCall::test("project.search", json!({}));
    let result = ToolResult {
        call_id: "c".into(),
        ok: true,
        output: big_output(),
        duration_ms: 1,
    };
    let cancel = CancellationToken::new();
    let patch = hook.post_tool_use(&ctx, &call, &result, &cancel).await.unwrap();
    assert!(patch.replace_output.is_none());
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo test -p mur-agent-runtime --test compress_hook`
Expected: FAIL to compile — `CompressHook` not found.

- [ ] **Step 4: Implement `CompressHook`**

Create `mur-agent-runtime/src/hooks/compress.rs`:

```rust
//! `CompressHook` — size-gated auto-compression of agent tool outputs (Surface 2).
//!
//! Mirrors `B0SafetyHook::post_tool_use`: returns a `PostToolUsePatch.replace_output`
//! so the supervisor can rewrite `ToolResult.output` before it is recorded / shown
//! to the agent. Like B0 rule 8, the end-to-end effect depends on the supervisor
//! consuming `replace_output` (tracked separately); the hook + chain folding are
//! complete and tested here.

use std::path::PathBuf;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use mur_compress::{CompressConfig, CompressEngine};

use crate::hooks::{Hook, HookCtx, HookError, PostToolUsePatch, ToolCall, ToolResult};

/// Auto-compresses oversized tool outputs for MUR's own spawned agents.
pub struct CompressHook {
    /// CCR store dir, i.e. `<mur_home>/compress`.
    dir: PathBuf,
    /// Loaded compression config (carries the `auto` gates).
    cfg: CompressConfig,
}

impl CompressHook {
    pub fn new(dir: PathBuf, cfg: CompressConfig) -> Self {
        Self { dir, cfg }
    }
}

#[async_trait::async_trait]
impl Hook for CompressHook {
    async fn post_tool_use(
        &self,
        _ctx: &HookCtx,
        _call: &ToolCall,
        result: &ToolResult,
        _tok: &CancellationToken,
    ) -> Result<PostToolUsePatch, HookError> {
        if !self.cfg.auto.enabled || !self.cfg.auto.agent_runtime {
            return Ok(PostToolUsePatch::default());
        }
        let text = match &result.output {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        // Per-call engine (cheap; mirrors the CLI/MCP per-call pattern and keeps
        // the hook `Send + Sync` without holding a non-Sync tokenizer).
        let engine = match CompressEngine::new(&self.dir, self.cfg.clone()) {
            Ok(e) => e,
            Err(_) => return Ok(PostToolUsePatch::default()),
        };
        let outcome = mur_compress::auto_compress(&engine, &text, None, self.cfg.auto.min_tokens);
        if !outcome.fired {
            return Ok(PostToolUsePatch::default());
        }
        let replacement = match outcome.hash {
            Some(_) => mur_compress::retrieval_envelope(&outcome, None),
            None => Value::String(outcome.text),
        };
        Ok(PostToolUsePatch {
            replace_output: Some(replacement),
        })
    }
}
```

- [ ] **Step 5: Wire module + re-export in `mod.rs`**

In `mur-agent-runtime/src/hooks/mod.rs`, add `pub mod compress;` after `pub mod b0_helpers;` (line 18), and add `pub use compress::CompressHook;` after `pub use b0::B0SafetyHook;` (line 26).

- [ ] **Step 6: Register in `build_chain`**

In `mur-agent-runtime/src/hooks/builder.rs`, immediately after the `let mut chain: Vec<Arc<dyn Hook>> = vec![ … ];` block ends (after line 31), insert:

```rust
    // Auto-compression of oversized tool outputs (Surface 2). Gated by
    // compress.yaml `auto.enabled` + `auto.agent_runtime`.
    let ccfg = mur_compress::CompressConfig::load(mur_home);
    if ccfg.auto.enabled && ccfg.auto.agent_runtime {
        chain.push(Arc::new(super::compress::CompressHook::new(
            mur_home.join("compress"),
            ccfg,
        )));
    }
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo test -p mur-agent-runtime --test compress_hook`
Expected: PASS (3 tests).

- [ ] **Step 8: Format, clippy, commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo fmt -p mur-agent-runtime
cargo clippy -p mur-agent-runtime -- -D warnings
git add mur-agent-runtime/src/hooks/compress.rs mur-agent-runtime/src/hooks/mod.rs mur-agent-runtime/src/hooks/builder.rs mur-agent-runtime/Cargo.toml mur-agent-runtime/tests/compress_hook.rs
git commit -m "feat(agent-runtime): CompressHook auto-compresses oversized tool outputs (Surface 2)"
```

---

## Task D: Teaching skill + README

**Files:**
- Create: `mur-core/src/skills/mur_compress.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs:1072-1110`
- Create: `mur-compress/README.md`

- [ ] **Step 1: Create the skill YAML**

Create `mur-core/src/skills/mur_compress.yaml`:

```yaml
name: mur-compress
version: 0.1.0
publisher: human:mur
description: "Read and recover auto-compressed tool outputs, and compress large text on demand. MUR shrinks big tool results automatically; the original is one mur_retrieve away."
category: context
hosts: [all]
content:
  abstract: |
    Large tool outputs may arrive auto-compressed as
    {compressed:true, content, hash, note}. Call `mur_retrieve` with the hash
    (and the same query, if any) to get the full original back.
  context: |
    # mur-compress — Reading and recovering compressed output

    MUR automatically compresses large tool outputs before you read them
    (search dumps, logs, diffs, big JSON). Compression is reversible.

    ## When a result is an envelope
    If a tool result looks like
    `{ "compressed": true, "content": …, "hash": "…", "note": … }`,
    the `content` is a shrunk view. To recover the full original:
    ```
    mur_retrieve(hash="…")              # full original
    mur_retrieve(hash="…", query="…")   # BM25-filtered to the most relevant items
    ```
    Pass the SAME query you used for the search: retrieval ranks the offloaded
    items by relevance so you pull back only what you need.

    ## Compress something yourself
    Before pasting a huge blob you control (a log, a giant JSON):
    ```
    mur_compress(content="…", query="optional focus")
    ```
    Returns the compressed text plus a hash for later retrieval.

    ## Don't fight the gate
    Small outputs are never compressed (below the token threshold), so most
    results are untouched. You only need `mur_retrieve` when you actually see a
    `compressed:true` envelope.

    ## Turning it off
    Configured in `~/.mur/compress.yaml` under `auto:` (`enabled`, `min_tokens`,
    `mcp`, `agent_runtime`). Set `enabled: false` to disable entirely.
tags: [mur, compress, tokens, retrieve, builtin]
triggers:
  - type: keyword
    pattern: "(compressed:true|mur_retrieve|mur_compress|auto-?compress|token compress)"
  - type: manual
priority: normal
```

- [ ] **Step 2: Register the skill in `ensure_mur_skill`**

In `mur-core/src/cmd/sync_cmd.rs`, inside the `skills` array (after the `mur-project-search` tuple, line 1092), add:

```rust
        (
            "mur-compress",
            include_str!("../skills/mur_compress.yaml"),
        ),
```

- [ ] **Step 3: Create the README**

Create `mur-compress/README.md`:

```markdown
# mur-compress

Reversible, content-aware token compression for MUR. Shrinks the bulk machine
text an LLM reads — search dumps, build logs, git diffs, large JSON — and offloads
the original to a local store keyed by hash.

> Design inspiration: [headroom](https://github.com/chopratejas/headroom) (Apache-2.0).
> Clean-room reimplementation — no headroom source is copied.

## What it does

A two-stage pipeline (reformat → offload) with content-type routing
(search / log / diff / json / generic). A result is only kept if it actually pays
off (the worth-it gate), so compression never inflates output.

## Manual use (MCP tools)

- `mur_compress(content, query?)` → `{ compressed, hash, … }`
- `mur_retrieve(hash, query?)` → full original, or BM25-filtered items when `query` is given
- `mur_compress_stats()` → cumulative tokens/cost saved

CLI equivalents: `mur compress [file] [--query q]`, `mur retrieve <hash> [--query q]`.

## Automatic use (`auto:`)

MUR auto-compresses large outputs on two LLM-facing surfaces — MCP tool results
and agent-runtime tool outputs — gated by `~/.mur/compress.yaml`:

```yaml
auto:
  enabled: true        # master switch
  min_tokens: 1500     # outputs smaller than this are never auto-compressed
  mcp: true            # MCP tool outputs
  agent_runtime: true  # agent post_tool_use outputs
```

When fired, the result becomes `{ compressed:true, content, hash, note }`; the
`note` tells the reader how to `mur_retrieve` the original. See the `mur-compress`
skill for the agent-facing guide.
```

- [ ] **Step 4: Verify the crate still builds (validates `include_str!` path)**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo build -p mur-core`
Expected: builds clean. A wrong YAML path would fail `include_str!` at compile time.

- [ ] **Step 5: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo fmt -p mur-core
git add mur-core/src/skills/mur_compress.yaml mur-core/src/cmd/sync_cmd.rs mur-compress/README.md
git commit -m "docs(compress): add mur-compress teaching skill + crate README"
```

---

## Task E: use-vs-no-use bench harness

**Files:**
- Create: `scripts/compress-bench.sh`

> **Note:** This task is run by the orchestrator in the main session after A–D are green (it needs the built `mur` binary). It is not a TDD subagent task. Requires `jq`.

- [ ] **Step 1: Write the harness**

Create `scripts/compress-bench.sh` (and `chmod +x`):

```bash
#!/usr/bin/env bash
# Use-vs-no-use compression benchmark. Drives the real `mur compress` CLI over a
# real + synthetic corpus, verifies reversibility, and writes results.json.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
MUR="${MUR:-$ROOT/target/release/mur}"
OUT="${OUT:-$ROOT/target/compress-bench}"
CORPUS="$OUT/corpus"
mkdir -p "$CORPUS"
: > "$OUT/rows.ndjson"

command -v jq >/dev/null || { echo "jq required" >&2; exit 1; }
[ -x "$MUR" ] || { echo "build mur first: cargo build --release -p mur-core" >&2; exit 1; }

# ---- real corpus ---------------------------------------------------------
rg --line-number "fn " mur-core/src 2>/dev/null | head -2000 > "$CORPUS/search.txt" || true
cargo build 2>&1 | head -2000 > "$CORPUS/build.log" || true
git log -p -n 3 > "$CORPUS/history.diff" || true
cargo metadata --format-version=1 > "$CORPUS/metadata.json" 2>/dev/null || true
"$MUR" project search "compression engine" --json > "$CORPUS/projsearch.json" 2>/dev/null || true

# ---- synthetic corpus ----------------------------------------------------
yes "DEBUG cache hit for key=session-token-abc123" | head -5000 > "$CORPUS/repetitive.log"
{ printf '['; for i in $(seq 1 5000); do printf '{"id":%d,"name":"item-%d","v":%d}' "$i" "$i" $((i*7)); [ "$i" -lt 5000 ] && printf ','; done; printf ']'; } > "$CORPUS/huge-array.json"
head -c 20000 /dev/urandom | base64 > "$CORPUS/dense.txt"   # incompressible ⇒ passthrough proof

# ---- measure -------------------------------------------------------------
for f in "$CORPUS"/*; do
  [ -s "$f" ] || continue
  name="$(basename "$f")"
  err="$("$MUR" compress "$f" 2>&1 >/dev/null)"   # stats line on stderr
  orig="$(printf '%s' "$err" | grep -oE '\[[0-9]+' | head -1 | tr -d '[')"
  comp="$(printf '%s' "$err" | grep -oE -- '-> [0-9]+' | head -1 | grep -oE '[0-9]+')"
  saved="$(printf '%s' "$err" | grep -oE '\([0-9.]+% saved' | grep -oE '[0-9.]+')"
  hash="$(printf '%s' "$err" | grep -oE 'hash=[a-f0-9]+' | cut -d= -f2 || true)"
  ok=true
  if [ -n "${hash:-}" ]; then
    "$MUR" retrieve "$hash" > "$OUT/rt.txt" 2>/dev/null || ok=false
    cmp -s "$f" "$OUT/rt.txt" || ok=false   # reversibility check
  fi
  jq -nc --arg n "$name" --argjson o "${orig:-0}" --argjson c "${comp:-0}" \
        --arg s "${saved:-0}" --arg h "${hash:-}" --argjson ok "$ok" \
    '{name:$n, orig_tokens:$o, comp_tokens:$c, saved_pct:($s|tonumber), hash:$h, reversible:$ok}' \
    >> "$OUT/rows.ndjson"
done

# ---- aggregate -----------------------------------------------------------
jq -s '{rows: ., totals: {orig: (map(.orig_tokens)|add), comp: (map(.comp_tokens)|add)}}
       | .totals.saved_pct = (if .totals.orig>0 then (100*(.totals.orig-.totals.comp)/.totals.orig) else 0 end)' \
   "$OUT/rows.ndjson" > "$OUT/results.json"

echo "Wrote $OUT/results.json"
jq -r '.rows[] | [.name, .orig_tokens, .comp_tokens, (.saved_pct|tostring)+"%", (if .reversible then "ok" else "FAIL" end)] | @tsv' "$OUT/results.json" \
  | column -t -s $'\t'
jq -r '.totals | "TOTAL  orig=\(.orig) comp=\(.comp) saved=\(.saved_pct|floor)%"' "$OUT/results.json"
```

- [ ] **Step 2: Build the release binary and run the harness**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo build --release -p mur-core
chmod +x scripts/compress-bench.sh
./scripts/compress-bench.sh
```
Expected: a printed table + `target/compress-bench/results.json`. The `dense.txt` row must show ~0% saved and `reversible: ok` (or no hash) — proving the worth-it gate passes incompressible input through. Search/log/array rows should show high savings with `reversible: ok`.

- [ ] **Step 3: Commit the harness**

```bash
git add scripts/compress-bench.sh
git commit -m "test(compress): add use-vs-no-use bench harness over real+synthetic corpus"
```

---

## Task F: deliverables (SVG + MD report)

**Files:**
- Create: `docs/mur-compress-autowire.svg`
- Create: `docs/mur-compress-autowire-report.md`

> **Note:** Run by the orchestrator after Task E. The SVG uses the `drawing-architecture-diagrams` skill; numbers come from `target/compress-bench/results.json`.

- [ ] **Step 1: Generate the SVG** via the `drawing-architecture-diagrams` skill: two surfaces (MCP `call_tool`, agent `CompressHook`) → `mur_compress::auto` facade (token gate + worth-it gate) → `compress()` → CCR store, with a savings-by-content-type bar panel populated from `results.json`. Clean, labeled arrows, no clutter.

- [ ] **Step 2: Write `docs/mur-compress-autowire-report.md`** with: a one-paragraph architecture recap, an embedded reference to the SVG, the use-vs-no-use comparison table (one row per corpus item: bytes/tokens before, tokens after, % saved, reversible), a per-content-type summary, and the Surface 2 supervisor caveat. Numbers must be copied from `results.json` (no invented figures).

- [ ] **Step 3: Commit**

```bash
git add docs/mur-compress-autowire.svg docs/mur-compress-autowire-report.md
git commit -m "docs(compress): autowire architecture SVG + use-vs-no-use comparison report"
```

---

## Final verification

- [ ] **Whole-workspace build + test + lint**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo build --workspace
cargo test -p mur-compress -p mur-mcp-server -p mur-agent-runtime
cargo clippy -p mur-compress -p mur-mcp-server -p mur-agent-runtime -- -D warnings
```
Expected: clean build, all tests pass, no clippy warnings.

- [ ] **Open a PR** (mur is PR-only to `main`): `gh pr create --base main --title "Auto-wire mur-compress into MCP + agent-runtime" --body "<summary + link to spec/report>"`

---

## Self-Review (completed by plan author)

**Spec coverage:** facade §4.1 → Task A. Surface 1 §4.2 → Task B. Surface 2 §4.3 → Task C. Config §5 → Task A (Step 3). Safety §6 → double gate in A + skip-list in B. Skill+README §7 → Task D. Test plan §8 → Task E (corpus, metrics, reversibility). Deliverables §9 → Task F. Build sequence §10 → task order A→[B,C,D]→E→F. ✔ All sections covered.

**Placeholder scan:** No TBD/TODO; all code blocks complete; commands have expected output. ✔

**Type consistency:** `AutoCfg{enabled,min_tokens,mcp,agent_runtime}`, `AutoOutcome{text,hash,original_tokens,compressed_tokens,fired}`, `auto_compress(engine,text,query,min_tokens)`, `retrieval_envelope(&outcome,query)`, `retrieval_note(hash,query)` used identically across Tasks A/B/C. `CompressEngine::{count_tokens,config}` defined in A, used in B/C. `ToolResult{call_id,ok,output,duration_ms}`, `ToolCall::test`, `HookCtx::for_test_with_home`, `PostToolUsePatch{replace_output}` match the real APIs read from the codebase. ✔

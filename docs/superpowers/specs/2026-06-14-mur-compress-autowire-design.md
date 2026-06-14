# Spec: Auto-wire `mur-compress` into MUR's LLM-facing paths

- **Date:** 2026-06-14
- **Status:** Approved design → ready for implementation plan
- **Topic:** Make the existing `mur-compress` engine fire **automatically** on the data MUR feeds an LLM, instead of only when the model manually calls `mur_compress`.
- **Predecessor:** `docs/superpowers/specs/2026-06-03-mur-compress-token-design.md` (the engine itself). This spec is the *wiring*, not the engine.

---

## 1. Summary

`mur-compress` already implements a reversible, content-aware compression engine exposed as three MCP tools (`mur_compress`, `mur_retrieve`, `mur_compress_stats`). It is **opt-in**: nothing compresses unless the model explicitly asks.

This change adds **automatic, size-gated compression** at the two surfaces that constitute "everything an LLM reads" in MUR:

1. **Surface 1 — MCP output path** (`mur-mcp-server`): what a Claude Code session reads from MUR's tools.
2. **Surface 2 — agent-runtime `post_tool_use` hook** (`mur-agent-runtime`): what MUR's *own* spawned agents read from their tool calls.

Both route through a new shared facade `mur_compress::auto`. Compression is **on by default** but protected by a **double gate** (token threshold + the existing worth-it ratio), so it never inflates output and never touches small structured results.

Two surfaces from the original brainstorm — **context injection** (low yield: Generic prose) and **session-recording** (format-invasive) — were **explicitly cut** and are Non-Goals here.

---

## 2. Motivation

The "headroom property" is that bulk machine text (search dumps, logs, diffs, large JSON) is compressed *before* it reaches the model, transparently, with the original one retrieval away. MUR has the engine but not the property: today the model must notice bloat and act. Automatic wiring delivers the savings the engine was built for without relying on model initiative.

The `2026-06-03` spec already names `mur-mcp-server/src/tools.rs` (`all_tools()`, `call_tool()`) as a sanctioned MUR integration point (§17), and defers only **proxy** and **agent-wrap** modes as Non-Goals. Auto-wiring the in-process MCP and agent-runtime paths is squarely within prior intent.

---

## 3. Goals / Non-Goals

### Goals
- A shared `mur_compress::auto` facade so each call site is thin and the gate logic is tested once.
- An `auto:` config section on `CompressConfig` (loads from `~/.mur/compress.yaml`), **default-on**, with a global kill switch and per-surface toggles.
- **Surface 1:** automatic compression of MCP tool outputs at the `call_tool` choke point, including **query-aware** compression for tools that took a `query` argument (free BM25-retrievable results for `mur_project_search`, `mur_notes_search`).
- **Surface 2:** a `CompressHook` in `mur-agent-runtime` that rewrites oversized `ToolResult.output` via `PostToolUsePatch::replace_output`.
- A **teaching skill** (`SKILL.md`, modeled on `mur-project-search`) + a `mur-compress/README.md` section.
- A **reproducible test harness** comparing token use with/without compression over a real + synthetic corpus, including a reversibility correctness check.
- Deliverables: a polished **SVG** architecture/savings diagram and an **MD** report with the comparison table.

### Non-Goals (explicitly cut or deferred)
- **Context-injection compression** (Surface 3) — cut. Generic prose yields little; budget truncation already bounds it.
- **Session-recording compression** (Surface 4) — cut. Format-invasive; harvest-pipeline coupling not worth the risk now.
- HTTP **proxy** / **agent-wrap** modes — deferred by the predecessor spec; unchanged.
- New compressors, ML/AST compression, vector CCR retrieval — out of scope; reuse the engine as-is.
- Changing CLI / human-facing output. Compression applies only on LLM-facing paths.

---

## 4. Design

### 4.1 Keystone — `mur_compress::auto` facade (build first)

New module `mur-compress/src/auto.rs`. All surfaces depend on it; nothing else depends on the surfaces.

```rust
// Config — serialized under CompressConfig.auto
pub struct AutoCfg {
    pub enabled: bool,        // default true   (global kill switch)
    pub min_tokens: usize,    // default 1500   (token gate; below this, never compress)
    pub mcp: bool,            // default true   (Surface 1)
    pub agent_runtime: bool,  // default true   (Surface 2)
}

pub struct AutoOutcome {
    pub text: String,             // compressed text, or the original if the gate/worth-it check declined
    pub hash: Option<String>,     // Some(_) only when content was offloaded to CCR
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub fired: bool,              // true iff we actually replaced the text
}

/// Token-gate, then delegate to engine.compress(). Never errors: any failure returns the original.
pub fn auto_compress(engine: &CompressEngine, text: &str, query: Option<&str>, min_tokens: usize) -> AutoOutcome;

/// Standard, model-readable hint appended when content was offloaded.
pub fn retrieval_note(hash: &str, query: Option<&str>) -> String;
```

**Gate responsibilities (no overlap):**
- The **caller** (surface code) checks `auto.enabled` and its per-surface flag (`auto.mcp` / `auto.agent_runtime`) *before* calling `auto_compress`. `auto_compress` is config-agnostic — it takes `min_tokens` explicitly and knows nothing about `enabled`.
- `auto_compress` itself:
  1. Count tokens. If `< min_tokens` → return original, `fired:false`.
  2. Call `engine.compress(text, query)`. The engine's existing **worth-it gate** (`bloat_threshold`) returns the input unchanged with `hash:none` if compression can't beat the threshold.
  3. If the engine produced a smaller result → `fired:true`, carry `hash`; else return original, `fired:false`. Never errors: any failure returns the original.

`CompressConfig` gains `pub auto: AutoCfg` (`mur-compress/src/config.rs:23`), with serde defaults so existing `compress.yaml` files keep working (missing `auto:` ⇒ defaults, i.e. on).

### 4.2 Surface 1 — MCP output path

**Anchor:** `mur-mcp-server/src/tools.rs` — `compress_engine()` helper at `:11`; `pub async fn call_tool(name, args)` match at `:343`; per-tool handlers through `:640`. Dispatch choke point in `mur-mcp-server/src/server.rs:108-116` serializes the returned `Value` to text for the model.

**Change:** Make `call_tool` a thin wrapper around the existing match.
1. Rename the current big match body to `async fn dispatch_tool(name, args) -> Result<Value, String>` (mechanical; no behavior change).
2. New `call_tool`:
   ```rust
   pub async fn call_tool(name: &str, arguments: &Value) -> Result<Value, String> {
       let out = dispatch_tool(name, arguments).await?;
       Ok(auto::maybe_compress_tool_output(name, arguments, out))
   }
   ```
3. `maybe_compress_tool_output` (lives in `mur-mcp-server`, uses the local `compress_engine()`):
   - Load config; if `!auto.enabled || !auto.mcp` → return `out` unchanged.
   - **Skip-list:** `mur_compress`, `mur_retrieve`, `mur_compress_stats` (never recompress the compression tools themselves).
   - Extract an optional query from `arguments["query"]` (string) → enables **query-aware** BM25-retrievable compression for search-style tools with zero per-handler edits.
   - Serialize `out` to text, `auto_compress(engine, &text, query, min_tokens)`.
   - If `fired`: return a compact JSON envelope, e.g.
     ```json
     { "compressed": true, "content": "<compressed text>", "hash": "…",
       "note": "Large output compressed. Call mur_retrieve(hash[, query]) for the full result." }
     ```
     else return `out` unchanged.

This single wrapper is **both** the query-aware search case and the universal net. `server.rs` is untouched. Engine construction already exists (`compress_engine()`), so no new wiring beyond the wrapper.

### 4.3 Surface 2 — agent-runtime `post_tool_use` hook

**Anchors:** `mur-agent-runtime/src/hooks/mod.rs` — `trait Hook` (`:51`), `post_tool_use(&self, ctx: &HookCtx, call: &ToolCall, result: &ToolResult, tok) -> Result<PostToolUsePatch, HookError>` (`:120`). `ToolResult.output: serde_json::Value` (`types.rs:263`). `PostToolUsePatch.replace_output: Option<serde_json::Value>` with `noop()`/`merge()` (`patch.rs:71-89`). Chain assembly in `builder.rs:25 build_chain(profile, agent_home, mur_home)`; existing `B0SafetyHook` already uses `replace_output` (redaction rule 8) — `CompressHook` is the direct parallel.

**Change:** New `mur-agent-runtime/src/hooks/compress.rs`:
- `CompressHook` holding the compress dir (`mur_home/compress`) + loaded `CompressConfig`.
- `post_tool_use`: if `!auto.enabled || !auto.agent_runtime` → `PostToolUsePatch::noop()`. Else serialize `result.output` to text, `auto_compress(...)` (no query available here), and on `fired` return `PostToolUsePatch { replace_output: Some(envelope_value) }` (same envelope shape as Surface 1) so the agent reads compressed output with a retrieval hint.
- Register in `build_chain` behind the `auto.agent_runtime` flag (and/or a `profile.hooks` tier toggle, matching how optional hooks are pushed at `builder.rs:34,67,74`).
- **Cargo:** add `mur-compress` dependency to `mur-agent-runtime`. `mur-compress` is a leaf crate (depends only on its own ccr/bm25/tokenizer) → no dependency cycle.

---

## 5. Config schema

`~/.mur/compress.yaml` gains an optional block (shown with defaults):

```yaml
auto:
  enabled: true        # master switch for all auto-compression
  min_tokens: 1500     # outputs smaller than this are never auto-compressed
  mcp: true            # Surface 1: MCP tool outputs
  agent_runtime: true  # Surface 2: agent post_tool_use outputs
```

Missing `auto:` ⇒ all defaults (on). Setting `enabled: false` ⇒ exact pre-change behavior (manual tools still work).

---

## 6. Safety / correctness

- **Never inflates.** Two gates: `min_tokens` (skip small) and the engine's `bloat_threshold` worth-it check (skip if compression doesn't pay). If neither passes, the original `Value` is returned byte-for-byte.
- **Reversible.** Offloaded content lives in the CCR store keyed by hash; the envelope always carries the hash + a `mur_retrieve` hint. Query-aware compression (Surface 1) stores items so BM25 retrieval works.
- **Structured-result protection.** Small JSON (status, counts) is below `min_tokens` → untouched. The compression tools are skip-listed → never double-compressed.
- **Instant rollback.** `auto.enabled: false` globally, or per-surface `mcp:`/`agent_runtime:`.
- **No CLI/human impact.** Wiring is on `call_tool` (MCP) and the agent hook only; `mur-core` `do_*` library functions and CLI rendering are unchanged.

---

## 7. Teaching skill + README

- **mur skill** `SKILL.md` (modeled on the `mur-project-search` skill so it appears in the learning index). Teaches:
  - Mental model: "tool outputs you read may already be compressed; the original is one `mur_retrieve` away."
  - How to read the envelope (`compressed:true`, `hash`, `note`).
  - When to pass `query` to `mur_retrieve` (BM25 filtering of offloaded items).
  - When to *manually* `mur_compress` (e.g., before pasting a huge blob you control).
  - The config knobs and how to disable.
- **`mur-compress/README.md`** section: condensed version of the above for repo readers, plus the headroom design credit already in `lib.rs:1`.

---

## 8. Test plan (use vs no-use)

Reproducible harness driving the **real `mur compress` CLI** (fidelity to production behavior).

- **Real corpus** (generated at run time from the `mur` repo): `rg` dump (search results), `cargo build`/`clippy` log, `git diff`, a large JSON config, and real `mur_project_search` output.
- **Synthetic corpus** (crafted edge cases): a huge JSON array, a highly-repetitive log, and an already-dense prose blob (to prove the worth-it gate passes through with 0 savings).
- **Per-item metrics:** content-type (as detected), bytes, tokens before, tokens after (tiktoken), % saved, $ saved at `stats.cost_per_mtok_usd`.
- **Correctness check:** for each compressed item, `mur retrieve <hash>` must reconstruct the original (assert equal). A row that can't round-trip is failed, not counted as savings.
- **Outputs:** machine-readable `results.json` (per-item + aggregate) → rendered into the MD table and the SVG.

---

## 9. Deliverables

- **SVG** (via drawing-architecture-diagrams skill): the two surfaces → `auto` facade → `compress()` → CCR store, annotated with measured % savings per content type. A clean, labeled, non-cluttered diagram.
- **MD report** `docs/mur-compress-autowire-report.md`: architecture recap, the use-vs-no-use comparison table, and a savings-by-content-type summary. Embeds/links the SVG.

---

## 10. Build sequence (maps to implementation units / subagents)

| Unit | Work | Depends on |
|------|------|-----------|
| **A** | `mur_compress::auto` facade + `AutoCfg` config + unit tests | — |
| **B** | Surface 1: `tools.rs` `dispatch_tool`/`call_tool` wrapper + query extraction + skip-list + tests | A |
| **C** | Surface 2: `CompressHook` + `build_chain` registration + Cargo dep + tests | A |
| **D** | Teaching `SKILL.md` + `mur-compress/README.md` section | A (for accurate config docs) |
| **E** | Test/comparison harness + corpus generation + `results.json` | A, B |
| **F** | SVG + MD report from `results.json` | E |

A is the keystone. B, C, D can proceed in parallel after A. E after A/B. F after E.

---

## 11. Risks / open questions

- **`min_tokens=1500` default** is a starting point; the harness (E) may suggest tuning. Tunable via config, so not load-bearing.
- **Envelope shape** (`{compressed, content, hash, note}`) should be consistent across both surfaces so the skill teaches one pattern. Pinned in the plan.
- **Agent retrieval path:** Surface 2 assumes MUR's agents can call `mur_retrieve`. If a given agent profile lacks the compress MCP tools, `agent_runtime:false` (or per-profile hook tiering) avoids handing it un-retrievable hashes. Plan to confirm during Unit C.
- **Double-compression across surfaces:** an agent calling an MCP tool could in principle hit both Surface 1 and Surface 2. The skip-list + `compressed:true` marker must short-circuit re-compression. Pinned in the plan.

---

## 12. References

- Engine: `mur-compress/src/lib.rs` (`CompressEngine::{new,compress,retrieve,stats_snapshot}`), `types.rs` (`CompressResult`), `config.rs:23` (`CompressConfig`), `auto.rs` (new).
- Surface 1: `mur-mcp-server/src/tools.rs:11,343,398,560-640`, `server.rs:108-116`.
- Surface 2: `mur-agent-runtime/src/hooks/{mod.rs:51,120, patch.rs:71, types.rs:260, builder.rs:25}`.
- Predecessor spec: `docs/superpowers/specs/2026-06-03-mur-compress-token-design.md` (§3 Non-Goals, §17 integration points).
- Design inspiration: headroom (https://github.com/chopratejas/headroom, Apache-2.0); clean-room, credited in `lib.rs:1`.

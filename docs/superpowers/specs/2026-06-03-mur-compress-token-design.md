# MUR Compress-Token — Design Spec

- **Date:** 2026-06-03
- **Status:** Approved (design) → ready for implementation planning
- **Author:** David Chang (with Claude Code)
- **Topic:** A native, local-first token-compression subsystem for MUR, exposed as MCP tools, modeled on the proven architecture of [`headroom`](https://github.com/chopratejas/headroom) (Apache-2.0) but reimplemented clean-room as a lean MIT crate.

---

## 1. Context & Motivation

AI coding agents (Claude Code, Codex, etc.) burn most of their context budget on **machine-generated bulk text**: grep/ripgrep dumps, build/test logs, git diffs, and large JSON payloads. `headroom` demonstrates that **content-type-aware, reversible compression** cuts this by 60–95% with no measured accuracy loss on standard benchmarks — and crucially, the largest real-world wins (code search ~92%, SRE log triage ~92%) come from **deterministic, structure-aware transforms**, not ML.

MUR already exposes interactive MCP tools (`mur-mcp-server`) and owns a retrieval pipeline (hybrid BM25 + vector). Adding a native compress/retrieve capability lets any MCP client shrink tool output before it reaches the LLM, while keeping originals locally retrievable — directly aligned with MUR's local-first positioning.

This spec defines a new `mur-compress` crate plus three MCP tools, deliberately scoped to deterministic offline transforms.

## 2. The Optimization Target — what "better compression" means

Compression quality is defined as **savings × task-accuracy × reversibility**, measured in **tokens** (not bytes). Three inviolable rules follow:

1. **Token-denominated.** All measurement, targets, and stop conditions use a real tokenizer, not byte length.
2. **Never corrupt the input.** A compressor that occasionally mangles a tool output is worse than no compression. Any uncertainty or error ⇒ return the original unchanged (fail-safe passthrough).
3. **Lossy-in-prompt, lossless-on-disk.** Anything dropped from the prompt is stored verbatim and retrievable on demand (CCR). The model fetches the full original only if it actually needs it.

## 3. Goals / Non-Goals

### Goals (v1)
- A `mur-compress` crate implementing a **two-stage transform pipeline** (Reformat → Offload) with reversible CCR storage.
- Four deterministic, content-aware compressors: **search, log, diff, json** + a safe **fallback**.
- A real tokenizer (`tiktoken-rs`) replacing MUR's `len/4` heuristic for this subsystem.
- Three MCP tools: `mur_compress`, `mur_retrieve`, `mur_compress_stats`.
- A thin optional CLI (`mur compress` / `mur retrieve`) for testing and manual use.
- Persistent, bounded CCR store under `~/.mur/compress/`.

### Non-Goals (explicitly deferred — YAGNI)
- HTTP **proxy** mode and **agent-wrap** mode (overlaps with MUR's existing positioning).
- **ML prose compression** (the Kompress-base / ModernBERT path) — heaviest dependency (ONNX/fastembed/hf-hub), smallest marginal win. Deferred to v2.
- **Code AST** compression (tree-sitter / ast-grep). v1 treats source code as fallback. Deferred to v2.
- **CacheAligner** (provider KV-cache prefix stabilization) — relevant to a proxy, not an ad-hoc tool. Deferred.
- Vector/embedding retrieval for CCR lookup. v1 uses BM25-only (offline, already available via `tantivy`).

## 4. Architecture — two-stage reversible pipeline

The core best-practice insight from headroom: **the first quality lever is content-type routing + format-specific transforms**, and high ratios come from a **two-stage** design where a safe lossless pass runs always, and a reversible lossy pass runs only when it pays off.

```
mur_compress(content, query?)
  │
  ├─ 1. detect_content_type(content)          # regex heuristics; ambiguous → Generic (passthrough)
  │
  ├─ 2. REFORMAT  (always run, lossless)       # re-encode same info more densely
  │        └─ roundtrip check: reconstruct == original, else discard reformat & passthrough
  │
  ├─ 3. estimate_bloat() > threshold ?         # cheap O(n) token-denominated gate
  │        └─ 4. OFFLOAD (reversible)          # drop low-value content → original/items into CcrStore
  │                 └─ if query present: keep top-K by BM25 relevance, offload the tail
  │
  └─ 5. measure with tiktoken-rs
        → { compressed, hash, tokens_before, tokens_after, tokens_saved,
            savings_percent, transforms[], note:"…hash=…" }

  Any error in any stage ⇒ return original content unchanged.
```

- **Reformat = lossless densification.** Always safe, always reversible by reconstruction, needs no store. Examples: JSON minify, log templating, search-result file grouping.
- **Offload = reversible drop.** Removes content the LLM probably won't read, **stashing the verbatim original (at item granularity) in the CcrStore** and leaving a `hash=` marker. This is where the big percentage comes from, but nothing is truly lost.
- **`estimate_bloat()` gate.** A cheap O(n) pre-pass (token-denominated) decides whether the offloader is worth engaging. Already-dense or tiny content is left alone — avoids latency and "helping backwards."
- **Query bias.** When a query (or inferable last-user-turn context) is present, the offloader keeps the most relevant items and offloads the rest — this is what lets "100 search hits → top-8 + retrieve-on-demand" preserve task accuracy.

## 5. Content detection (`detect.rs`)

Deterministic regex/heuristic classifier (no ML, no `magika` in v1). Returns a `ContentType`; on any ambiguity returns `Generic`.

| `ContentType` | Heuristic (illustrative) |
|---|---|
| `SearchResults` | high fraction of lines matching `^\S+:\d+:` (grep/rg `file:line:content`) |
| `BuildLog` | high fraction of lines matching timestamp/log-level patterns (`\b(ERROR|WARN|INFO|DEBUG|TRACE)\b`, ISO-8601/epoch stamps) |
| `GitDiff` | presence of `^diff --git`, `^@@ .* @@`, `^--- a/`, `^+++ b/` |
| `Json` | content parses as JSON (array or object) after trimming |
| `Generic` | anything else (prose, source code, mixed) → fallback compressor |

Thresholds are **config-driven constants** (see §11), never hardcoded magic numbers inline.

## 6. Transform traits (`transform.rs`)

Two small traits mirror headroom's proven Reformat/Offload split, but use MUR-native types:

```rust
/// Lossless densification. Must be reconstructable: a Reformat is only
/// accepted if reformat→reconstruct reproduces the original byte-for-byte.
pub trait Reformat: Send + Sync {
    fn name(&self) -> &'static str;
    fn applies_to(&self) -> &[ContentType];
    fn apply(&self, content: &str) -> Result<ReformatOutput, CompressError>;
}

/// Reversible drop. Stashes the verbatim original (item-granular) in the
/// store and returns compressed text plus the retrieval hash.
pub trait Offload: Send + Sync {
    fn name(&self) -> &'static str;
    fn applies_to(&self) -> &[ContentType];
    /// Cheap O(n) token-denominated estimate of how much is droppable.
    fn estimate_bloat(&self, content: &str, tok: &dyn TokenCounter) -> f32;
    fn apply(
        &self,
        content: &str,
        ctx: &CompressCtx,        // query, target_ratio, protect budget
        store: &dyn CcrStore,
    ) -> Result<OffloadOutput, CompressError>;
}
```

The orchestrator (`lib.rs`) runs detection → reformat (with roundtrip guard) → bloat gate → offload, then measures. The pipeline is **synchronous and deterministic**.

## 7. The four compressors + fallback (`compressors/`)

Each compressor = a Reformat stage (always) + an optional Offload stage (gated).

| Compressor | Reformat (lossless) | Offload (reversible → CCR) | Typical savings |
|---|---|---|---|
| **search** | group hits by file; fold repeated `path:` prefixes; relative line numbers | with query, keep top-K hits by BM25; offload the rest as items | 85–92% |
| **log** | template extraction `<TS> <LVL> <MSG>` + varying-field table; merge repeated templates with counts | drop INFO/DEBUG noise & duplicates; keep ERROR/WARN + head/tail; full log → store | 80–92% |
| **diff** | drop redundancy recomputable from hunk headers; keep ± lines + headers | offload large unchanged context blocks, leave anchors; full diff → store | 50–80% |
| **json** | minify; hoist shared keys into a schema | SmartCrusher-style: collapse repeated array rows → schema + sample + count; offload long-array tail; original array → store | 70–95% |
| **fallback** | strip trailing whitespace; collapse repeated blank lines | none by default (passthrough); prose/code left for v2 | 0–20% |

Each compressor caps work by a **token target ratio** and a **protect budget** (head/tail or recent items always preserved), both config-driven.

## 8. Tokenizer (`tokenizer.rs`)

- `TokenCounter` trait with a `tiktoken-rs` implementation using `cl100k_base` (embedded in the crate; **fully offline, no network**).
- Used as a **Claude approximation**: Anthropic does not publish its tokenizer; headroom uses the same approximation. The reported counts are documented as estimates.
- A `len/4` fallback counter remains available for environments where the vocab fails to load, so the subsystem degrades rather than fails.

## 9. CCR reversible store (`ccr/store.rs`)

- **Location:** `~/.mur/compress/` (resolved via `mur_home()` in `mur-common`, honoring `$MUR_HOME`).
- **Write:** atomic temp-file + rename (same pattern as `mur-core/src/store/yaml.rs`).
- **Key:** `blake3(original)[:24]` hex (24 chars / 96 bits). `blake3` added as a dep; if we prefer zero new hash deps, `sha2` (already a `mur-core` dep) is an acceptable substitute — decided at implementation time, behavior identical.
- **Granularity:** store **structured items** (parsed search hits / log line-groups / diff hunks / JSON rows), not one opaque blob, so retrieval can BM25-filter *within* a stored original and return only relevant items.
- **Entry schema (serialized):**
  ```
  hash, content_type, created_at, ttl,
  original_text, items[]            # parsed items for query-filtered retrieve
  original_tokens, compressed_tokens, item_count, last_accessed, retrieval_count
  ```
- **Payload compression at rest:** optional `flate2`/`zstd` to keep `~/.mur/compress/` small (originals can be large). `flate2` exists in `mur-agent-runtime`; it (or `zstd`) is added to this crate.
- **Eviction:** persistent (survives restarts) + **TTL (default 7 days)** + **size cap (default: max entries / max bytes)** + LRU. Expiry checked on access; opportunistic sweep on write. All limits config-driven.

## 10. MCP tools (`mur-mcp-server`)

Mirror headroom's tool surface (familiar to agents already using `headroom_*`). Added by **two edits in `mur-mcp-server/src/tools.rs`**: a `Tool {}` literal in `all_tools()` and a match arm in `call_tool()`. No changes to `server.rs`/`jsonrpc.rs`.

### `mur_compress`
- **Input:** `{ "content": string (required), "query": string (optional) }`
- **Output (JSON):** `{ compressed, hash, original_tokens, compressed_tokens, tokens_saved, savings_percent, transforms: [..], note: "Original stored with hash=<hash>. Use mur_retrieve to fetch full content." }`

### `mur_retrieve`
- **Input:** `{ "hash": string (required), "query": string (optional) }`
- **Behavior:** O(1) store lookup. With `query`, BM25-score the stored items and return top-K above threshold; without `query`, return the full original.
- **Output (JSON):** full → `{ hash, content_type, original_content, item_count }`; filtered → `{ hash, query, results: [..], count }`; miss → `{ error, hash, hint }`.

### `mur_compress_stats`
- **Input:** none.
- **Output (JSON):** `{ compressions, retrievals, total_input_tokens, total_output_tokens, total_tokens_saved, savings_percent, estimated_cost_saved_usd, store: { entries, bytes, max_entries } }`. Cost estimate uses a **config-driven $/M-token rate** (not a hardcoded constant).

## 11. Configuration

New section in `~/.mur/config.yaml` (all values have defaults; nothing hardcoded inline):

```yaml
compress:
  enabled: true
  tokenizer: cl100k_base          # tiktoken-rs vocab
  target_ratio: 0.30              # aim to reach <=30% of original tokens
  bloat_threshold: 0.20           # engage offloader only if >=20% estimated droppable
  protect_head_lines: 20
  protect_tail_lines: 20
  retrieve_top_k: 20
  retrieve_score_threshold: 0.30  # BM25 cutoff for query-filtered retrieve
  detect:
    search_min_ratio: 0.6
    log_min_ratio: 0.5
  store:
    dir: ~/.mur/compress
    ttl_days: 7
    max_entries: 2000
    max_bytes: 536870912          # 512 MiB
    compress_at_rest: true
  stats:
    cost_per_mtok_usd: 3.0
```

## 12. CLI (optional, `mur-core` → `cmd/compress.rs`)

- `mur compress [--query Q] [FILE|-]` → prints compressed text + the hash marker; stores original.
- `mur retrieve <hash> [--query Q]` → prints original or filtered items.
- Thin wrappers over the same `mur-compress` API. May be cut without affecting the MCP surface.

## 13. Crate structure

```
mur-compress/                 # new workspace member (8th crate), MIT
  Cargo.toml
  src/
    lib.rs                    # orchestrator: detect → reformat → gate → offload → measure
    detect.rs                 # ContentType heuristics
    transform.rs              # Reformat / Offload traits, CompressCtx, outputs, CompressError
    tokenizer.rs              # TokenCounter trait + tiktoken-rs impl + len/4 fallback
    compressors/
      mod.rs
      search.rs
      log.rs
      diff.rs
      json.rs
      fallback.rs
    ccr/
      mod.rs
      store.rs                # CcrStore: item-granular, blake3 key, TTL+LRU, atomic write
      entry.rs                # CompressedEntry schema
    stats.rs                  # savings tracker (atomic JSON, models savings_tracker.py)
    config.rs                 # serde view over the `compress:` config section
  tests/
    fixtures/                 # MUR-authored sample inputs per content type
    roundtrip.rs              # reformat reconstruct == original
    reversibility.rs          # offload → retrieve == original
    ratios.rs                 # savings within expected bands per content type
```

Each source file stays under the **800-line** project limit; split into submodules if a compressor grows.

## 14. Dependencies

| Dep | Status | Use |
|---|---|---|
| `serde`, `serde_json`, `serde_yaml` | workspace | entry/config (de)serialization |
| `regex` | present (mur-core) | detection, templating |
| `tantivy` | present (mur-core) | BM25 for query-filtered retrieve |
| `tempfile` | present | atomic writes / tests |
| `tiktoken-rs` | **add** | offline token counting (`cl100k_base`) |
| `flate2` or `zstd` | **add (to this crate)** | payload-at-rest compression |
| `blake3` | **add (optional)** | content-address hashing (or reuse `sha2`) |

No ONNX / fastembed / magika / hf-hub / tree-sitter in v1 — keeps the crate offline and light.

## 15. Correctness strategy & licensing

headroom is **Apache-2.0**; this workspace is **MIT**. To keep licensing clean:

- **Clean-room reimplementation.** The compressors are written from the *documented behavior and algorithm ideas* described in this spec, **not** by copying headroom source. headroom is credited as design inspiration in code comments / a `CREDITS` note, which Apache-2.0 §4 attribution welcomes.
- **MUR-authored fixtures.** Test fixtures are original inputs we write to model the same scenarios (code search, SRE log, git diff, JSON array), not copied from headroom's fixture corpus.
- If, during implementation, any headroom code or data is copied verbatim, that file is marked Apache-2.0 with a `NOTICE` attribution — but the default is clean-room MIT.

This is a deliberate, documented adjustment to the originally-discussed "borrow their parity fixtures" idea (avoids Apache→MIT entanglement).

## 16. Error handling & fail-safe

- Every stage returns `Result<_, CompressError>`; the orchestrator catches any error and returns the **original content unchanged** with `transforms: []` and zero savings. Compression is best-effort and must never break a caller.
- Reformat stages are guarded by a **roundtrip assertion** (reconstruct == original); a failed assertion discards the reformat rather than risking lossy "lossless" output.
- Retrieve on an expired/missing hash returns a structured `{ error, hint }`, never panics.

## 17. Testing strategy

- **Roundtrip:** every Reformat reconstructs its input exactly (property-style over fixtures).
- **Reversibility:** for every Offload, `retrieve(hash)` returns the verbatim original; `retrieve(hash, query)` returns the relevant items.
- **Ratio bands:** each content type compresses within an expected savings band on fixtures (regression guard against quality drift).
- **Fail-safe:** malformed/edge inputs (truncated JSON, binary, empty, huge single line) return passthrough, never error.
- **Tokenizer:** counts match `tiktoken-rs` reference values on known strings; fallback path activates when vocab is absent.
- **MCP integration:** extend `mur-mcp-server/tests/integration.rs` to cover the three new tools end-to-end.

## 18. Confirmed defaults (from design review)

1. **Tokenizer:** `tiktoken-rs` `cl100k_base` as a documented Claude approximation.
2. **Storage:** persistent on disk (cross-session) with TTL 7 days + size cap + LRU.
3. **prose/code:** v1 passthrough (fallback compressor only); AST + ML prose are v2.
4. **CLI:** included but cuttable.

## 19. Future (v2+)

- ML prose compression (Kompress-base or equivalent) behind a feature flag.
- Code-AST compression (tree-sitter).
- Internal MUR use: compress context-injection / retrieved patterns / session recordings before they reach an agent.
- Optional vector-scored retrieve (reuse `score_and_rank_hybrid`).
- `magika`-backed content detection if regex heuristics prove insufficient.

## 20. References

- headroom repo (Apache-2.0): https://github.com/chopratejas/headroom — `headroom/transforms/`, `headroom/ccr/`, `headroom/integrations/mcp/server.py`, `crates/headroom-core/src/transforms/pipeline/`.
- MUR integration points: `mur-mcp-server/src/tools.rs` (`all_tools()`, `call_tool()`), `mur-core/src/store/yaml.rs` (atomic write), `mur-core/src/retrieve/scoring.rs` (BM25/hybrid), `mur-common` `mur_home()`.

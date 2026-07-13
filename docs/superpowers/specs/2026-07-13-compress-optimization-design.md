# Compress Optimization: JSON Deep Collapse + Generic Early Skip

**Date:** 2026-07-13
**Status:** Approved (design review with user)
**Crate:** `mur-compress`

## Motivation

Production stats (`~/.mur/compress/stats.json`, cumulative by_type):

| type | compressions | input tokens | saved | ratio |
|---|---|---|---|---|
| git_diff | 1,060 | 4.9M | 4.3M | 89.2% |
| build_log | 45 | 119K | 78K | 65.7% |
| search_results | 5,194 | 6.9M | 2.3M | 33.4% |
| **json** | **467** | **1.7M** | **28K** | **1.7%** |
| **generic** | **8,328** | **20.1M** | **9K** | **~0%** |

Two failure modes:

1. **JSON:** `compressors/json.rs` only collapses a *top-level* array. Real MCP/tool
   payloads are objects wrapping large arrays or long strings (`{"results": [...]}`,
   `{"content": "..."}`), which today get nothing but minification. Headroom's
   SmartCrusher claims 60–95% on the same shapes.
2. **Generic:** 63% of all compressions run through the whitespace-only fallback and
   save ~0%, wasting CPU and inflating stats with no-op "compressions".

## Scope

- **In:** recursive JSON collapse (arrays at any depth + long string leaves),
  query-aware BM25 sampling, generic early-skip with exact savings pre-computation,
  `skipped` stats counter.
- **Out:** code skeletonization in the main pipeline (breaks `Edit` old_string
  anchoring on fresh file reads — `skeleton.rs` stays cc-proxy-supersession-only);
  new content-type detectors; changes to `mur_retrieve` semantics.

## Part 1: JSON deep collapse (`compressors/json.rs`)

### Behavior

Walk the parsed `serde_json::Value` tree. Two collapsible node kinds:

1. **Large array** (any depth, `len >= MIN_ARRAY_FOR_COLLAPSE` (4) and
   `len > sample_n`): replace in place with the existing collapse shape
   `{_schema, _total, _shown, sample, _note}`. `_schema` from the first
   object element's keys (empty for non-object elements), `_note` carries the hash.
2. **Long string leaf** (token count >= new config `json.max_string_tokens`,
   default 200): truncate to a head slice plus a
   `"...[N tokens elided, hash=H]"` suffix.

### Sampling

- With `ctx.query`: serialize each array element to its JSON string, rank with the
  existing `bm25::bm25_rank`, keep top-`sample_n` **in original array order**
  (same pattern as `compressors/search.rs`).
- Without query: head `sample_n` (current behavior). `sample_n` remains
  `cfg.protect_head_lines.min(len)`.

### Offload semantics (unchanged)

One `store.put_original(content, items, ContentType::Json, tok)` call for the
**whole original document** → one hash. Every collapse note in the tree references
that same hash. `mur_retrieve` is untouched and returns the original byte-for-byte.
The original is stored **before** any tree mutation; failure at any collapse step
falls back to plain minify (current behavior). No data-loss path exists.

### Safety rails

- Recursion depth cap (constant, e.g. 64) — deep/hostile nesting degrades to
  minify-only below the cap, never panics or overflows the stack.
- The engine's existing payoff gate (passthrough when `tokens_saved == 0`) is the
  final guard; unchanged.
- New transforms recorded: `json.deep_collapse`, `json.string_elide` (observable in
  stats/transforms alongside the existing `json.minify` / `json.row_collapse`).

### Config

```yaml
json:
  max_string_tokens: 200   # string leaves at/above this get elided
```

Added to `CompressConfig` with serde default; absent config behaves as default.

## Part 2: Generic early skip (engine path in `lib.rs` + `stats.rs`)

### Behavior

The generic fallback can only ever save `trailing_ws_bytes + excess_blank_lines`.
That is **exactly computable** in one cheap scan before compressing:

1. detect → `ContentType::Generic`
2. compute potential savings ratio (bytes-based estimate over the same scan the
   fallback would do)
3. if ratio < `fallback.min_save_ratio` (new config, default 0.05) → **passthrough**:
   no compressor run, no CCR write, recorded as `skipped` in stats
4. else → existing fallback compression, recorded as today

Other content types are untouched.

### Stats semantics fix

Today a no-op passthrough still increments `compressions` (hence 8,328 × ~0%).
Add a `skipped` counter (top-level + per version/day bucket). Skipped calls
increment `skipped` only. `by_type` numbers then reflect real compression work.
Existing stats.json files deserialize cleanly (serde default 0 for the new field).

### Config

```yaml
fallback:
  min_save_ratio: 0.05   # generic inputs predicted to save less than 5% are skipped
```

## Error handling (both parts)

- JSON parse failure → `CompressError::Parse` → engine falls back (unchanged).
- Any collapse failure → minify-only output (unchanged fallback), original already
  in CCR when a hash is emitted.
- Skip path cannot error: it is a pure scan + passthrough.

## Testing

`cargo nextest` (workspace convention; plain `cargo test` is flaky):

- **json:** nested-object large-array collapse; long-string elide; BM25 sampling
  picks query-relevant elements (order preserved); depth cap degrades to minify;
  retrieve returns original byte-for-byte; short/small inputs still plain-minify.
- **generic skip:** low-savings input passes through untouched and increments
  `skipped`; high-savings input still compresses; stats round-trip with the new
  field; old stats.json (no `skipped`) still loads.
- All existing tests stay green.

## Expected impact

- json: 1.7% → 40%+ on object-wrapped payloads (directionally; measured post-ship
  via by_type stats).
- generic: ~8K no-op compressions/period stop burning CPU and polluting stats.
- No behavior change for git_diff / search_results / build_log paths.

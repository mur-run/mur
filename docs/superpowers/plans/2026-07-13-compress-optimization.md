# Compress Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** JSON deep collapse (nested arrays + long strings, query-aware sampling) and generic early-skip with honest `skipped` stats.

**Architecture:** All changes inside the `mur-compress` crate. Task 1 adds config knobs; Task 2 adds the `skipped` stats counter; Task 3 rewrites the JSON compressor as a recursive tree walk; Task 4 wires the generic pre-scan skip into the engine. Tasks 3 and 4 are independent of each other; both depend on 1–2.

**Tech Stack:** Rust edition 2024, serde_json, existing `bm25::bm25_rank`, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-07-13-compress-optimization-design.md`

## Global Constraints

- Single source file ≤ 800 lines.
- No hardcoded values — new knobs go in `CompressConfig` with serde defaults.
- Test with `cargo nextest run -p mur-compress` (plain `cargo test --workspace` is flaky in this repo).
- `mur_retrieve` semantics unchanged: one hash per compressed document, original returned byte-for-byte.
- Old `stats.json` files must keep deserializing (`#[serde(default)]` on every new field).
- Commit after every task.

---

### Task 1: Config knobs (`json.max_string_tokens`, `fallback.min_save_ratio`)

**Files:**
- Modify: `mur-compress/src/config.rs`

**Interfaces:**
- Produces: `CompressConfig.json: JsonCfg { max_string_tokens: usize }` (default 200); `CompressConfig.fallback: FallbackCfg { min_save_ratio: f32 }` (default 0.05). Later tasks read `ctx.config.json.max_string_tokens` and `config.fallback.min_save_ratio`.

- [ ] **Step 1: Write the failing test** — append to the existing `#[cfg(test)]` module in `config.rs`:

```rust
#[test]
fn new_sections_default() {
    let cfg = CompressConfig::default();
    assert_eq!(cfg.json.max_string_tokens, 200);
    assert!((cfg.fallback.min_save_ratio - 0.05).abs() < f32::EPSILON);
}

#[test]
fn yaml_without_new_sections_still_loads() {
    // Simulates an existing user compress.yaml predating these fields.
    let cfg: CompressConfig = serde_yaml::from_str("enabled: true\n").unwrap();
    assert_eq!(cfg.json.max_string_tokens, 200);
    assert!((cfg.fallback.min_save_ratio - 0.05).abs() < f32::EPSILON);
}
```

(If `config.rs` has no test module, create one mirroring a sibling's shape. If `serde_yaml` isn't a dev-dependency, check `Cargo.toml`; the crate parses YAML config in `load` so it is a normal dependency already — use whatever the existing `load` tests use.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-compress config`
Expected: FAIL — `no field 'json' on CompressConfig`(compile error counts as the failing state).

- [ ] **Step 3: Implement** — in `config.rs`, next to `DetectCfg`/`StoreCfg` (match their derive/default style exactly):

```rust
/// `json:` section — knobs for the JSON deep-collapse compressor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JsonCfg {
    /// String leaves whose token count is at/above this get elided
    /// (head kept, remainder offloaded).
    pub max_string_tokens: usize,
}

impl Default for JsonCfg {
    fn default() -> Self {
        Self { max_string_tokens: 200 }
    }
}

/// `fallback:` section — knobs for the Generic path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FallbackCfg {
    /// Generic inputs whose exactly-computable savings ratio is below this
    /// are skipped (passthrough) instead of compressed.
    pub min_save_ratio: f32,
}

impl Default for FallbackCfg {
    fn default() -> Self {
        Self { min_save_ratio: 0.05 }
    }
}
```

Add to `CompressConfig` (with `#[serde(default)]` like `auto`):

```rust
    #[serde(default)]
    pub json: JsonCfg,
    #[serde(default)]
    pub fallback: FallbackCfg,
```

And add `json: JsonCfg::default(), fallback: FallbackCfg::default(),` to `CompressConfig`'s `Default` impl (check how it's written — derive vs manual — and follow it).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p mur-compress config`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add mur-compress/src/config.rs
git commit -m "feat(compress): json/fallback config sections (max_string_tokens, min_save_ratio)"
```

---

### Task 2: `skipped` stats counter

**Files:**
- Modify: `mur-compress/src/stats.rs`

**Interfaces:**
- Consumes: existing `StatsTracker::update` lock-and-merge helper.
- Produces: `StatsTracker::record_skip(&self, content_type: &str)`; `skipped: u64` on `StatsData`, `BucketData`, `TypeStats`, `StatsSnapshot`. Task 4 calls `record_skip`.

- [ ] **Step 1: Write the failing test** — append to the `#[cfg(test)]` module in `stats.rs`:

```rust
#[test]
fn record_skip_counts_separately() {
    let dir = tempfile::tempdir().unwrap();
    let t = StatsTracker::new(dir.path().join("stats.json"));
    t.record_compression("generic", 100, 50);
    t.record_skip("generic");
    t.record_skip("generic");
    let snap = t.snapshot(3.0, 0, 0);
    assert_eq!(snap.compressions, 1);
    assert_eq!(snap.skipped, 2);
    assert_eq!(snap.by_type.get("generic").unwrap().skipped, 2);
    // compressions untouched by skips
    assert_eq!(snap.by_type.get("generic").unwrap().compressions, 1);
}

#[test]
fn old_stats_json_without_skipped_loads() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("stats.json");
    std::fs::write(&p, r#"{"compressions":5,"retrievals":1,"total_input_tokens":10,"total_output_tokens":5,"total_tokens_saved":5}"#).unwrap();
    let t = StatsTracker::new(p);
    let snap = t.snapshot(3.0, 0, 0);
    assert_eq!(snap.compressions, 5);
    assert_eq!(snap.skipped, 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-compress stats`
Expected: FAIL — no `skipped` field / no `record_skip`.

- [ ] **Step 3: Implement**

Add `#[serde(default)] skipped: u64` to `StatsData`, and `#[serde(default)] pub skipped: u64` to `BucketData` and `TypeStats`. Add `pub skipped: u64` to `StatsSnapshot` and populate it in `snapshot()` from the data (mirror how `compressions` flows through).

Add next to `record_retrieval`:

```rust
    /// Record a deliberate skip (early-skip gate): the input was inspected
    /// and passed through untouched. Counted separately from compressions
    /// so by_type ratios reflect real compression work.
    pub fn record_skip(&self, content_type: &str) {
        let day = today_key();
        self.update(|d| {
            d.skipped += 1;
            d.buckets
                .entry(current_version().to_string())
                .or_default()
                .entry(day.clone())
                .or_default()
                .skipped += 1;
            d.by_type.entry(content_type.to_string()).or_default().skipped += 1;
        });
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p mur-compress stats`
Expected: PASS (including the pre-existing stats tests).

- [ ] **Step 5: Commit**

```bash
git add mur-compress/src/stats.rs
git commit -m "feat(compress): skipped counter in stats (top-level, bucket, by_type)"
```

---

### Task 3: JSON deep collapse + string elide + query-aware sampling

**Files:**
- Modify: `mur-compress/src/compressors/json.rs` (full rewrite of the compress path; keep the module doc style)

**Interfaces:**
- Consumes: `bm25::bm25_rank(query, &[String]) -> Vec<(usize, f32)>`; `CcrStore::put_original(&self, content, items, ContentType, tok) -> Result<String, _>`; `JsonCfg.max_string_tokens` from Task 1.
- Produces: same `compress(content, ctx, store, tok) -> Result<CompressOutput, CompressError>` signature (engine dispatch unchanged). New transforms: `json.deep_collapse`, `json.string_elide`.

**Design notes for the implementer:**
- One hash for the whole original document. The hash isn't known until `put_original`, but notes are written during the walk — so write the sentinel `__MUR_HASH__` into notes during the walk and string-replace it with the real hash after storing. Store only if the walk collapsed anything.
- `items` passed to `put_original` = every collapsed array element (serialized) plus every elided full string, so query-filtered `mur_retrieve` can BM25 over them.
- Depth cap 64: below the cap the walk simply stops recursing (nodes deeper than the cap are left untouched); never panic.
- Elide keeps the head `max_string_tokens * 2` characters (`chars().take`, char-boundary safe; ~2 chars/token is a conservative cross-language ratio).

- [ ] **Step 1: Write the failing tests** — replace the test module additions in `json.rs` (keep the two existing tests; they must still pass):

```rust
    fn store_and_ctx(cfg: &CompressConfig) -> (tempfile::TempDir, CcrStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        (dir, store)
    }

    #[test]
    fn collapses_nested_array() {
        let cfg = CompressConfig { protect_head_lines: 2, ..Default::default() };
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx { query: None, config: &cfg };
        let input = r#"{"ok":true,"results":[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5},{"id":6}]}"#;
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_some(), "nested array must trigger offload");
        assert!(out.compressed.contains("_total"));
        assert!(out.transforms.iter().any(|t| t == "json.deep_collapse"));
        // retrieve returns the original byte-for-byte
        let got = store.get(out.hash.as_ref().unwrap()).unwrap().unwrap();
        assert_eq!(got.original_text, input);
    }

    #[test]
    fn elides_long_string_leaf() {
        let cfg = CompressConfig {
            json: crate::config::JsonCfg { max_string_tokens: 10 },
            ..Default::default()
        };
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx { query: None, config: &cfg };
        let long = "word ".repeat(200);
        let input = serde_json::json!({"content": long}).to_string();
        let out = compress(&input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_some());
        assert!(out.transforms.iter().any(|t| t == "json.string_elide"));
        assert!(out.compressed.len() < input.len() / 2);
        assert!(out.compressed.contains("elided"));
    }

    #[test]
    fn query_picks_relevant_sample() {
        let cfg = CompressConfig { protect_head_lines: 2, ..Default::default() };
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx { query: Some("zebra"), config: &cfg };
        let mut rows: Vec<serde_json::Value> =
            (0..10).map(|i| serde_json::json!({"id": i, "name": "common"})).collect();
        rows.push(serde_json::json!({"id": 99, "name": "zebra special"}));
        let input = serde_json::Value::Array(rows).to_string();
        let out = compress(&input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.compressed.contains("zebra"), "BM25 sample must include the query hit");
    }

    #[test]
    fn depth_cap_degrades_to_minify() {
        let cfg = CompressConfig::default();
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx { query: None, config: &cfg };
        // 80 levels of nesting, no large arrays: must not panic, plain minify.
        let mut s = String::new();
        for _ in 0..80 { s.push_str(r#"{"a":"#); }
        s.push('1');
        for _ in 0..80 { s.push('}'); }
        let out = compress(&s, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_none());
    }

    #[test]
    fn no_sentinel_leaks_into_output() {
        let cfg = CompressConfig { protect_head_lines: 2, ..Default::default() };
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx { query: None, config: &cfg };
        let input = r#"{"results":[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5}]}"#;
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(!out.compressed.contains("__MUR_HASH__"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-compress json`
Expected: FAIL — nested array / string elide produce no hash (current code only handles top-level arrays).

- [ ] **Step 3: Implement** — rewrite `json.rs` body (keep `MIN_ARRAY_FOR_COLLAPSE = 4`):

```rust
const MAX_DEPTH: usize = 64;
const HASH_SENTINEL: &str = "__MUR_HASH__";

/// Walk state: collected offload items + which transforms fired.
struct Walk<'a> {
    ctx: &'a CompressCtx<'a>,
    tok: &'a dyn TokenCounter,
    items: Vec<String>,
    collapsed_array: bool,
    elided_string: bool,
}

impl Walk<'_> {
    fn visit(&mut self, v: &mut serde_json::Value, depth: usize) {
        if depth >= MAX_DEPTH {
            return;
        }
        match v {
            serde_json::Value::Array(arr) => {
                let sample_n = self.ctx.config.protect_head_lines.min(arr.len());
                if arr.len() >= MIN_ARRAY_FOR_COLLAPSE && arr.len() > sample_n {
                    *v = self.collapse_array(std::mem::take(arr), sample_n);
                    self.collapsed_array = true;
                } else {
                    for item in arr {
                        self.visit(item, depth + 1);
                    }
                }
            }
            serde_json::Value::Object(map) => {
                for (_, val) in map.iter_mut() {
                    self.visit(val, depth + 1);
                }
            }
            serde_json::Value::String(s) => {
                let max = self.ctx.config.json.max_string_tokens;
                if self.tok.count(s) >= max {
                    self.items.push(s.clone());
                    // ~2 chars/token: conservative head slice, char-boundary safe.
                    let head: String = s.chars().take(max * 2).collect();
                    let elided = s.chars().count().saturating_sub(head.chars().count());
                    *v = serde_json::Value::String(format!(
                        "{head}...[{elided} chars elided, hash={HASH_SENTINEL}]"
                    ));
                    self.elided_string = true;
                }
            }
            _ => {}
        }
    }

    /// Replace a large array with {_schema,_total,_shown,sample,_note}.
    fn collapse_array(&mut self, arr: Vec<serde_json::Value>, sample_n: usize) -> serde_json::Value {
        let keys: Vec<String> = match arr.first() {
            Some(serde_json::Value::Object(m)) => m.keys().cloned().collect(),
            _ => Vec::new(),
        };
        let serialized: Vec<String> = arr
            .iter()
            .map(|x| serde_json::to_string(x).unwrap_or_default())
            .collect();

        // Query-aware sampling: BM25 top-N in original order; else head-N.
        let mut idx: Vec<usize> = match self.ctx.query {
            Some(q) => {
                let mut top: Vec<usize> = bm25_rank(q, &serialized)
                    .into_iter()
                    .take(sample_n)
                    .map(|(i, _)| i)
                    .collect();
                if top.is_empty() {
                    (0..sample_n).collect()
                } else {
                    top.sort_unstable();
                    top
                }
            }
            None => (0..sample_n).collect(),
        };
        idx.dedup();
        let sample: Vec<serde_json::Value> = idx.iter().map(|&i| arr[i].clone()).collect();

        self.items.extend(serialized);
        serde_json::json!({
            "_schema": keys,
            "_total": arr.len(),
            "_shown": sample.len(),
            "sample": sample,
            "_note": format!("{} rows collapsed; full array hash={HASH_SENTINEL}", arr.len() - sample.len()),
        })
    }
}

pub fn compress(
    content: &str,
    ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let mut val: serde_json::Value =
        serde_json::from_str(content.trim()).map_err(|e| CompressError::Parse(e.to_string()))?;
    let minified = serde_json::to_string(&val).map_err(|e| CompressError::Parse(e.to_string()))?;
    let mut transforms = vec!["json.minify".to_string()];

    let mut walk = Walk { ctx, tok, items: Vec::new(), collapsed_array: false, elided_string: false };
    walk.visit(&mut val, 0);

    if !walk.collapsed_array && !walk.elided_string {
        return Ok(CompressOutput { compressed: minified, hash: None, transforms });
    }

    // Offload the whole original once; every note points at this hash.
    let hash = store
        .put_original(content, walk.items, ContentType::Json, tok)
        .map_err(|e| CompressError::Store(e.to_string()))?;
    if walk.collapsed_array {
        transforms.push("json.deep_collapse".to_string());
    }
    if walk.elided_string {
        transforms.push("json.string_elide".to_string());
    }
    let compressed = serde_json::to_string(&val)
        .unwrap_or(minified)
        .replace(HASH_SENTINEL, &hash);

    Ok(CompressOutput { compressed, hash: Some(hash), transforms })
}
```

Add `use crate::bm25::bm25_rank;` to the imports. Delete the old top-level-array-only block (`collapse` behavior is preserved: a top-level array is just depth-0 of the walk — the existing `collapses_long_array` test must still pass; note its collapse output is now the walk's shape, adjust that test's assertions only if a field name genuinely changed — it shouldn't).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p mur-compress json`
Expected: PASS — all new tests plus the two pre-existing ones.

- [ ] **Step 5: Run the full crate suite**

Run: `cargo nextest run -p mur-compress`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-compress/src/compressors/json.rs
git commit -m "feat(compress): JSON deep collapse — nested arrays, string elide, query-aware BM25 sampling"
```

---

### Task 4: Generic early-skip in the engine

**Files:**
- Modify: `mur-compress/src/compressors/fallback.rs` (add `saving_ratio`)
- Modify: `mur-compress/src/lib.rs` (skip gate in `compress()`)

**Interfaces:**
- Consumes: `FallbackCfg.min_save_ratio` (Task 1), `StatsTracker::record_skip` (Task 2).
- Produces: `fallback::saving_ratio(content: &str) -> f32` — exact byte-level ratio the whitespace fallback would save.

- [ ] **Step 1: Write the failing tests**

In `fallback.rs` tests:

```rust
    #[test]
    fn saving_ratio_exact() {
        // "abc   \n\n\n\nxyz\n": 3 trailing ws + 2 excess blank lines = 5 of 14 bytes
        let s = "abc   \n\n\n\nxyz\n";
        let expect = 5.0 / s.len() as f32;
        assert!((saving_ratio(s) - expect).abs() < 1e-6);
        assert_eq!(saving_ratio("clean\ntext\n"), 0.0);
    }
```

In `lib.rs` tests (or the crate's existing engine test location — follow the sibling pattern):

```rust
    #[test]
    fn generic_low_savings_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CompressEngine::new(dir.path(), CompressConfig::default()).unwrap();
        // Prose with nothing to strip: predicted savings 0 < 5% → skip.
        let content = "plain prose line\n".repeat(200);
        let r = engine.compress(&content, None);
        assert_eq!(r.tokens_saved, 0);
        assert!(r.transforms.iter().any(|t| t == "skipped"));
        let snap = engine.stats_snapshot();
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.compressions, 0);
    }

    #[test]
    fn generic_high_savings_still_compresses() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CompressEngine::new(dir.path(), CompressConfig::default()).unwrap();
        // Heavy trailing whitespace: well above 5% → real compression.
        let content = "x                                        \n".repeat(300);
        let r = engine.compress(&content, None);
        assert!(r.tokens_saved > 0);
        assert!(r.transforms.iter().any(|t| t == "fallback.whitespace"));
    }
```

(Check `stats_snapshot()`'s exact signature in `lib.rs` — it may take no args and use config cost; use it as the existing engine tests do.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-compress fallback generic`
Expected: FAIL — `saving_ratio` undefined; skip path missing.

- [ ] **Step 3: Implement**

`fallback.rs` — one scan, no allocation:

```rust
/// Exact fraction of bytes the whitespace fallback would remove.
/// This is a precise pre-computation, not a heuristic: the fallback only
/// ever strips trailing whitespace and blank runs beyond the first line.
pub fn saving_ratio(content: &str) -> f32 {
    if content.is_empty() {
        return 0.0;
    }
    let mut saved = 0usize;
    let mut blank_run = 0usize;
    for line in content.lines() {
        let trimmed = line.trim_end();
        saved += line.len() - trimmed.len();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                saved += 1; // the '\n' of a dropped blank line
            }
        } else {
            blank_run = 0;
        }
    }
    saved as f32 / content.len() as f32
}
```

`lib.rs` — in `compress()`, right after the `enabled` check, before building `ctx`:

```rust
        // Generic early-skip: the fallback's savings are exactly computable
        // in one scan. Below the configured floor, don't run the compressor
        // at all — record a skip (not a compression) and pass through.
        if ct == ContentType::Generic
            && fallback::saving_ratio(content) < self.config.fallback.min_save_ratio
        {
            self.stats.record_skip(ct.as_str());
            let mut r = Self::passthrough(content, before, ct);
            r.transforms = vec!["skipped".to_string()];
            return r;
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p mur-compress`
Expected: PASS — new tests plus the whole crate suite.

- [ ] **Step 5: Workspace sanity + lint**

Run: `cargo clippy -p mur-compress -- -D warnings && cargo fmt --check`
Expected: clean. (Full-workspace build needs `MUR_WEB_DIST`/`ORT_STRATEGY` env; not required for this crate alone.)

- [ ] **Step 6: Commit**

```bash
git add mur-compress/src/compressors/fallback.rs mur-compress/src/lib.rs
git commit -m "feat(compress): generic early-skip — exact savings pre-scan, honest skipped stats"
```

---

## Verification (post-implementation)

1. `cargo nextest run -p mur-compress` — all green.
2. Manual smoke: `echo '{"results":[...20 rows...]}' | mur compress` shows `_total`/hash; `mur compress --file <prose.txt>` reports skip/no-op.
3. After a day of real use, `~/.mur/compress/stats.json` shows `json` ratio rising and `generic` compressions ≈ 0 with `skipped` accumulating.

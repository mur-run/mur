# mur Conversations Phase 3.5 — LLM-Abstractive Hit Compression (Stage 1b)

**Status:** Design approved 2026-04-22. Ready for plan.
**Depends on:** Phase 3.4 shipped (`958b48c`), Phase 3.2.1 shipped (`03eab9e`).
**Branch:** `fix/conversations-phase-3-5`, worktree at `/Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-5`.

---

## 1. Goal

Add a new overflow-loop stage that uses an Ollama LLM to **abstractively summarize retrieved hits** when Phase 3.4's heuristic extractive compression (Stage 1) isn't enough to fit the token budget. Stage 1b preserves `--continue` history turns at the cost of paid LLM latency on overflow queries; a file-per-key content cache makes this a one-time cost per hit.

## 2. Non-goals

- CLI flags (`--no-summarize`, `--summarize-model`) — config-only surface in 3.5.
- Configurable timeout (`summarize_timeout_ms`) — hardcoded 5s.
- Configurable target tokens per hit — internal math from overshoot.
- Max-hits cap — natural bound via early-exit.
- Cache eviction (TTL, LRU, cron sweep) — manual `rm -rf` only.
- Cache path sharding (2-char prefix dirs) — flat layout for now.
- LLM-abstractive compression of history turns — Stage 2 keeps drop-oldest semantics; hits only in 3.5.
- Batched multi-hit LLM calls (rejected in Q2).
- ONNX LLMLingua-2 or query-cosine sentence scoring (separate phases if ever).
- Context-aware trigger (Option A/B pick at runtime) — single cascade, always Option A.
- Migrating cache to LanceDB / SQLite.
- `mur conversations cache-clear` subcommand — manual `rm -rf ~/.mur/conversations/cache/abstractive/`.
- Streaming partial summaries.

## 3. Architecture

Phase 3.5 adds **Stage 1b** between existing Stage 1 (heuristic-compress) and Stage 2 (drop history) in `ask/prompt.rs::render`. New cascade:

```
Stage 1   heuristic-compress hits       (free, Phase 3.4)
Stage 1b  LLM-abstractive hits          (paid, Phase 3.5 NEW)
Stage 2   drop oldest history turn      (existing)
Stage 3   drop tail hits                (existing)
```

Stage 1b is sequential per-hit, largest-first, with early-exit when budget fits. Each call wraps a 5s timeout and soft-fails (warn + keep original). Results cache to `~/.mur/conversations/cache/abstractive/<sha256>.txt`, keyed by `sha256("mur-abstract-v1" || model || target_tokens || content)`. Model defaults to `answer_model` unless `summarize_model` is set.

### 3.1 Locked design choices (Q1–Q7)

| # | Question | Choice |
|---|---|---|
| Q1 | Cascade position | Option A — new Stage 1b between Stage 1 and Stage 2. Preserve history at all costs. |
| Q2 | Granularity | Option B — sequential per-hit, largest-first, re-measure after each, early-exit when under budget. |
| Q3 | Model | Option (b) — new `summarize_model: Option<String>`; `None` → falls through to `answer_model`. |
| Q4 | Cache | Refined (a) — file-per-key, atomic temp+rename, version-prefix in key (`"mur-abstract-v1"`), no automatic eviction. |
| Q5 | Failure mode | Option (a) — per-hit soft-fail, 5s hardcoded timeout, warn-and-continue, fall through to Stage 2 if still over. |
| Q6 | Provenance | Option (g) — plain marks abstractive only with `(summarized)` suffix; JSON carries `compressed: "heuristic" \| "abstractive" \| null` per citation; footer adds `· N summarized` aggregate. |
| Q7 | Config surface | Option (a) — 2 new fields: `summarize_hits_enabled: bool` (default `true`), `summarize_model: Option<String>` (default `None`). |

## 4. File structure

| File | Change | Responsibility |
|---|---|---|
| `mur-core/src/conversations/ask/abstractive.rs` | **NEW** | Stage 1b orchestrator: per-hit sequential compression, timeout, cache integration, provenance annotation |
| `mur-core/src/conversations/ask/cache.rs` | **NEW** | File-per-key cache helpers: `cache_key`, `cache_get`, `cache_put`. Pure I/O, no LLM knowledge |
| `mur-core/src/conversations/ask/prompt.rs` | modify | Insert Stage 1b call between existing Stage 1 and Stage 2 in `render`. Re-measure after |
| `mur-core/src/conversations/ask/format.rs` | modify | Citation suffix for `Some(Abstractive)`; footer aggregate `· N summarized` |
| `mur-core/src/conversations/ask/mod.rs` (or wherever `Citation`/`ResolvedHit` lives) | modify | Add `compressed: Option<Compression>` field; define `enum Compression { Heuristic, Abstractive }` |
| `mur-common/src/config.rs` | modify | Add `AskConfig.summarize_hits_enabled: bool` + `summarize_model: Option<String>` |
| `mur-core/src/conversations/ollama.rs` | modify | Extend `mock_generate` with abstractive-prompt branch + `MUR_ABSTRACTIVE_MOCK_FAIL` env hook |
| `mur-core/src/conversations/ask/compress.rs` | modify (tiny) | Tag Phase 3.4 heuristic output with `Compression::Heuristic` on affected hits |
| `mur-core/tests/cli_conversations.rs` | modify | 4 new integration tests (fires on overflow, disabled, cache hit, soft-fail) |
| `scripts/golden-path-conversations.sh` | modify | Add Step 18 (`mur ask --json` overflow with `.stage_1b.compressed > 0` assertion); final line becomes `=== ALL 18 STEPS GREEN ===` |

## 5. Stage 1b algorithm (data flow)

**Entry:** After Stage 1 (heuristic) runs in `prompt::render`, re-measure tokens. If `cur_tokens > max_context_tokens` AND `cfg.summarize_hits_enabled`, invoke Stage 1b.

**Per-invocation algorithm:**

1. Sort hits by serialized-char length, largest first. These are the candidates for summarization.
2. For each candidate in order:
   1. Compute `overshoot = cur_tokens - max_context_tokens`.
   2. If `overshoot <= 0`, break (early-exit).
   3. Compute `target_tokens_per_hit = max(60, current_hit_tokens - ceil(overshoot / remaining_candidates))`. Floor of 60 prevents over-aggressive compression.
   4. Compute `cache_key = sha256("mur-abstract-v1" || model || target_tokens || hit.content)`.
   5. Cache lookup: read `~/.mur/conversations/cache/abstractive/<cache_key>.txt`. If present, use it and skip to step 8.
   6. Ollama call: wrap in `tokio::time::timeout(Duration::from_secs(5))`. Prompt template = `ABSTRACTIVE_PROMPT_V1` (see §6).
   7. Validate response: non-empty, strictly shorter than original, no markdown fences/preamble. On failure: `tracing::warn!`, keep original, continue to next candidate.
   8. Apply: mutate `hit.snippet` = new text, set `hit.compressed = Some(Compression::Abstractive)`, write cache.
   9. Re-measure tokens. Update `cur_tokens`. Goto step 2.i.

**Exit:** Loop ends when `cur_tokens <= max_context_tokens` OR all candidates exhausted. Re-measure one more time; if still over, fall through to Stage 2 (drop oldest history turn).

**Worst-case latency:** `N_candidates × 5s` if every call times out. Typical: 1–3 LLM calls (overshoot is usually small), so 1–10s added on cold cache; ~0s on warm cache.

**Invariant:** `hit.compressed` flag is additive. A hit already marked `Some(Heuristic)` in Stage 1 gets upgraded to `Some(Abstractive)` in Stage 1b (the later transformation wins the provenance marker).

## 6. New modules — abstractive.rs + cache.rs

### 6.1 `ask/cache.rs`

```rust
pub fn cache_dir() -> PathBuf { mur_home().join("conversations/cache/abstractive") }

pub fn cache_key(model: &str, target_tokens: usize, content: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"mur-abstract-v1|");
    h.update(model.as_bytes()); h.update(b"|");
    h.update(target_tokens.to_le_bytes());
    h.update(b"|"); h.update(content.as_bytes());
    hex::encode(h.finalize())  // 64-char lowercase hex
}

pub fn cache_get(key: &str) -> Option<String>
pub fn cache_put(key: &str, value: &str) -> Result<()>
```

- All read-time errors → `None` (log debug, treat as miss).
- Writes use `temp + rename` pattern (same idiom as `store/yaml.rs`).
- `cache_dir()` is created on first `cache_put`.
- No LLM knowledge in this module — pure K/V over filesystem.

### 6.2 `ask/abstractive.rs`

```rust
pub struct AbstractiveCtx<'a> {
    pub client: &'a OllamaClient,
    pub model: &'a str,
    pub timeout: Duration,  // fixed 5s
}

pub enum CompressOutcome {
    Compressed,
    CacheHit,
    Skipped(String),  // reason: timeout | empty | not_shorter | ollama_err
}

pub async fn compress_hit(
    ctx: &AbstractiveCtx<'_>,
    hit: &mut ResolvedHit,
    target_tokens: usize,
) -> CompressOutcome;

pub async fn run_stage_1b(
    ctx: &AbstractiveCtx<'_>,
    hits: &mut [ResolvedHit],
    budget: &mut TokenBudget,
) -> Stage1bSummary;

pub struct Stage1bSummary {
    pub processed: usize,
    pub compressed_count: usize,   // freshly compressed (not from cache)
    pub cache_hits: usize,
    pub skipped: Vec<(usize, String)>,   // (hit_idx, reason) — drives warn logs
    pub duration_ms: u64,
}

// Derived at render time by dropping the Vec into a count:
pub struct Stage1bStats {
    pub compressed_count: usize,
    pub cache_hits: usize,
    pub skipped_count: usize,
    pub duration_ms: u64,
}
```

`skipped` reasons emitted via `tracing::warn!` at call site in `prompt::render`. The serialized `Stage1bStats` (see §8.5) is built from `Stage1bSummary` by replacing the `skipped` Vec with its length — keeps the JSON surface minimal while the runtime summary retains per-hit detail for logging.

### 6.3 Prompt template (hardcoded `ABSTRACTIVE_PROMPT_V1`)

```
System: You compress text for retrieval context. Preserve entities,
dates, numbers, and decisions. Do not add facts. Output only the
summary — no preamble, no markdown.

User: Summarize the following in ≤{target_tokens} tokens.

{content}
```

Prompt-version string `"mur-abstract-v1"` is baked into cache keys. Bump to `"v2"` when changing the template (invalidates all prior cache entries without needing a migration).

### 6.4 Separation rationale

- `cache.rs` is pure I/O and reusable for future features.
- `abstractive.rs` holds all Stage 1b semantics (prompt, validation, timeout, cache integration).
- `prompt::render` just calls `run_stage_1b` and re-measures — doesn't know about caching or Ollama.

## 7. Config wiring

In `mur-common/src/config.rs`:

```rust
#[serde(default = "default_true")]
pub summarize_hits_enabled: bool,

#[serde(default)]  // defaults to None
pub summarize_model: Option<String>,
```

Existing `~/.mur/config.yaml` files remain valid without edits; Stage 1b turns on by default (like Phase 3.4's `compress_hits_enabled`).

**Effective model resolution** (used once in `prompt::render` to build `AbstractiveCtx`):

```rust
fn effective_summarize_model<'a>(cfg: &'a AskConfig) -> &'a str {
    cfg.summarize_model.as_deref().unwrap_or(&cfg.answer_model)
}
```

**CLI flags:** none added. No environment-variable overrides beyond the existing serde → env pipeline.

**User config example:**

```yaml
ask:
  summarize_hits_enabled: true
  summarize_model: qwen3:4b   # power-user opt-in; omit → uses answer_model
```

## 8. Provenance surface

### 8.1 Shared data model

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    Heuristic,   // Phase 3.4
    Abstractive, // Phase 3.5
}

// In Citation (or ResolvedHit):
#[serde(skip_serializing_if = "Option::is_none")]
pub compressed: Option<Compression>,
```

### 8.2 Plain mode

Only `Some(Abstractive)` adds a suffix. Heuristic stays unmarked (preview remains a verbatim excerpt).

```
[cit: cc/c1 @L1] — "Discussed moving rollup job to overnight cron…" (summarized)
[cit: cc/c2 @L1] — "Found bug in MMR dedupe threshold; raised to 0.95"
```

### 8.3 JSON mode

Every citation carries `compressed` with all three values visible (`"abstractive"`, `"heuristic"`, or key omitted for `None`):

```json
{
  "citations": [
    { "cit": "cc/c1 @L1", "preview": "...", "compressed": "abstractive" },
    { "cit": "cc/c2 @L1", "preview": "...", "compressed": "heuristic" },
    { "cit": "cc/c3 @L1", "preview": "..." }
  ]
}
```

### 8.4 Footer

Current: `(5 hits · 920ms · 1800→1600 tokens)`
New when Stage 1b fired: `(5 hits · 2 summarized · 920ms · 1800→1600 tokens)`. The `· N summarized` segment appears only when N > 0. Counts abstractive only (heuristic not counted — matches plain-mode philosophy).

### 8.5 AskResponse

```rust
pub stage_1b: Option<Stage1bStats>,  // None if not fired
```

`Stage1bStats` as defined in §6.2 — carries `compressed_count`, `cache_hits`, `skipped_count`, `duration_ms`. Serialized under `"stage_1b": {...}` in JSON only.

## 9. Error handling / failure modes

Every Stage 1b failure falls through to "warn + keep original + continue." Failure taxonomy:

| Failure | Detection | Action |
|---|---|---|
| Ollama HTTP error (connection refused, 5xx) | `OllamaClient::generate` returns `Err` | `tracing::warn!`, `Skipped("ollama_err")`, keep original |
| Timeout (5s per call) | `tokio::time::timeout` elapses | `tracing::warn!`, `Skipped("timeout")`, keep original |
| Empty response | Response string is empty after trim | `Skipped("empty")`, keep original |
| Not shorter | Response `.len() >= original.len()` | `Skipped("not_shorter")`, keep original |

After all candidates processed, if `cur_tokens` still > `max_context_tokens`, fall through to Stage 2 (drop oldest history). The ask query always succeeds — Stage 1b is never the source of a hard error.

## 10. Testing strategy

### 10.1 Unit tests (in `ask/abstractive.rs` + `ask/cache.rs`)

- `cache_key_is_stable` — same inputs → same hex key across runs.
- `cache_key_version_prefix_changes_key` — identical inputs but different `"mur-abstract-v1"` prefix → different keys.
- `cache_key_differs_by_target_tokens` / `cache_key_differs_by_model`.
- `cache_put_then_get_roundtrip`.
- `cache_get_missing_returns_none`.
- `cache_put_is_atomic` — induce mid-write crash, verify final file either absent or complete.
- `compress_hit_respects_timeout` — mock Ollama sleeps 10s, 5s ctx timeout → `Skipped("timeout")`.
- `compress_hit_skips_when_not_shorter` — mock echoes input → `Skipped("not_shorter")`.
- `compress_hit_skips_on_empty_response` — mock returns `""` → `Skipped("empty")`.
- `compress_hit_writes_cache_on_success` — second call for same inputs hits cache.
- `run_stage_1b_early_exits_when_fit` — 5 hits, budget fits after 2 → `processed == 2`.

### 10.2 Integration tests (in `mur-core/tests/cli_conversations.rs`)

- `mur_ask_stage_1b_fires_on_overflow` — tight budget, footer contains `summarized`, JSON citation has `compressed: "abstractive"`.
- `mur_ask_stage_1b_disabled_via_config` — `summarize_hits_enabled: false` → no `summarized` marker.
- `mur_ask_stage_1b_cache_hits_on_second_run` — two consecutive asks over same archive, assert second's `.stage_1b.cache_hits > 0` in JSON output.
- `mur_ask_stage_1b_soft_fails_gracefully` — `MUR_ABSTRACTIVE_MOCK_FAIL=timeout`, ask still succeeds with originals + fall-through.

### 10.3 Mock infrastructure

Extend `ollama::mock_generate`:

- New prompt-content branch: if prompt contains `"You compress text for retrieval context"`, return `first 40 chars of input + " [mock summary]"`. Deterministic, strictly shorter, satisfies `not_shorter` validator.
- New env hook: `MUR_ABSTRACTIVE_MOCK_FAIL=timeout|empty|garbage` returns the requested failure mode instead of the canned summary.

### 10.4 Golden path

Existing `./scripts/golden-path-conversations.sh` (17 steps) adds **Step 18**: `mur ask --json` with a tight budget, assert `.stage_1b.compressed_count > 0`. Final line becomes `=== ALL 18 STEPS GREEN ===`.

## 11. Success criteria

1. Overflow queries that previously lost history (Stage 2 fires under Phase 3.4) now preserve history in the common case (Stage 1b fits budget without reaching Stage 2).
2. Stage 1b cold-cache latency ≤ 15s worst case (3 hits × 5s timeout); warm-cache near-zero.
3. Stage 1b soft-fail never causes ask to error — always falls through to existing cascade.
4. `(summarized)` marker appears in plain output when and only when LLM rewrote a hit.
5. Golden path extended to 18 steps, all green.
6. Users running Phase 3.4 workflows see zero behavior change on non-overflow queries (pure addition).

# mur Conversations Phase 3.4 — Heuristic Extractive Compression Design

**Status:** Approved 2026-04-22
**Depends on:** Phase 3.3 shipped (merge `3844b4f`; base PR `b860305`).
**Phase 3.5+ upgrade path:** Real LLMLingua-2 via ONNX (`ort` crate + ~130MB weights); Ollama-based compression using a small model; query-cosine sentence scoring.

---

## 1. Purpose

Phase 3.3 shipped `mur ask --continue` with a rolling 3-turn chat history feeding the generation prompt. Combined with the Phase 3.2 retrieval stack (collapsed-tree over layers 1-4) and the existing hits budget, prompts frequently approach the `max_context_tokens` ceiling. Phase 3.3's overflow response is strictly lossy — it drops the oldest history turn first, then shrinks hits by dropping lowest-scored ones. Both operations discard information wholesale.

Phase 3.4 inserts a less-lossy first response: heuristic extractive compression of hit snippets. Each hit's snippet is split into sentences; sentences are scored by position + query-keyword overlap; low-scoring (filler) sentences are dropped while preserving citation anchors. The hit still exists in the output, just with a shorter snippet.

Compression is **conditional** (fires only on overflow) and **pure heuristic** (no ML, no new dependencies, deterministic). Under-budget queries output byte-identical prompts to Phase 3.3 — zero regression surface.

## 2. Non-goals

Explicitly deferred or declined:

- **Real LLMLingua-2 via ONNX.** Requires `ort` crate + `tokenizers` crate + ~130MB model download on first run. High infrastructure cost for a local-first CLI. Deferred to Phase 3.5+ if measurement justifies.
- **Ollama-based compression** (small-model summarization of hits). Adds N extra LLM calls per `mur ask` (~1-3s latency). Consistent with existing stack but a measurable UX cost.
- **Query-cosine sentence scoring.** Would embed each sentence and compute cosine against query embedding. 3s per-ask overhead for marginal quality gain over position + keyword inside already-topically-aligned hits.
- **Graph centrality** (LexRank / TextRank). Designed for multi-document summarization where sentences cross-reference; degenerate on short single-hit passages.
- **Always-compress** (unconditional). Costs information on under-budget queries with no payoff. Conditional (overflow-only) is strictly better.
- **Per-knob config surface** (exposing `position_weight`, `jaccard_weight`, etc. to users). Algorithm-stability risk — Phase 3.5 may swap the heuristic for ONNX, at which point those fields become dead config. YAGNI.
- **CLI flag** for compression (`--no-compress`). Config boolean is enough; a flag duplicates surface for the same function.

## 3. Architecture

```
ask_stream / ask()
  │
  └── prompt::render(question, prior_turns, hits, max_tokens, resp_tokens, compress_enabled)
        │
        ├── Build ctx + user (full hits, full history)
        ├── Compute tokens_est
        │
        ├── Stage 1 — compress hits (once, only if over budget AND compress_enabled)
        │     compressed_hits = compress::compress_hits(hits, query, target_chars_per_hit)
        │     re-render user + recompute cur_tokens
        │
        ├── Stage 2 — drop oldest history turn (loop; existing Phase 3.3 behavior)
        │
        └── Stage 3 — shrink hits from tail (drop lowest-scored; existing Phase 3.3 behavior)
```

**File structure:**

| File | Role | Status |
|---|---|---|
| `mur-core/src/conversations/ask/compress.rs` | `compress_hits` + sentence splitter + scorer + tokenizer + stopwords | **New** (~120 LOC) |
| `mur-common/src/config.rs` | `AskConfig.compress_hits_enabled: bool = true` | Modify |
| `mur-core/src/conversations/ask/mod.rs` | `pub mod compress;` + `AskRequest.compress_enabled: bool` + `AskResponse` unchanged | Modify |
| `mur-core/src/conversations/ask/prompt.rs` | `render` gains `compress_enabled` param + Stage 1 integration + helper-fn extraction | Modify |
| `mur-core/src/cmd/conversations_cmd.rs` | wire `ask_cfg.compress_hits_enabled` into `AskRequest` | Modify |
| `mur-core/tests/cli_conversations.rs` | +1 integration test (tight budget triggers compression) | Modify |

No new Cargo dependencies. No LanceDB schema changes. No commander sync changes.

**Tech stack:** Rust 2024 · no new crates · pure heuristic · deterministic output.

## 4. Compression module (`ask/compress.rs`)

### 4.1 Public API

```rust
use super::retrieve::ResolvedHit;

/// Module-level tuning constants. Hardcoded per §6 Q6 — no config surface.
pub(crate) const COMPRESS_MIN_SENTENCES: usize = 4;
pub(crate) const COMPRESS_MIN_CHARS: usize = 400;
pub(crate) const POSITION_WEIGHT: f64 = 0.7;
pub(crate) const JACCARD_WEIGHT: f64 = 0.3;
pub(crate) const MIN_SENTENCES_PER_HIT: usize = 1;

/// Compress each hit's snippet to its top-scoring sentences.
///
/// - Hits failing the "worth compressing" threshold (see SKIP rules) pass
///   through unchanged.
/// - Eligible hits keep top-K sentences by position + jaccard score, subject
///   to `target_chars_per_hit` (soft cap).
/// - Preserves hit ordering + citation anchor metadata; only `snippet` shrinks.
/// - Floor: always emits ≥1 sentence per hit (citation invariant).
/// - Deterministic: same (hits, query, target) → same output.
pub fn compress_hits(
    hits: Vec<ResolvedHit>,
    query: &str,
    target_chars_per_hit: usize,
) -> Vec<ResolvedHit>
```

### 4.2 Sentence splitting

Byte-walking splitter, no regex. Breaks on `. ` / `! ` / `? ` / `\n\n`. Preserves separator tokens so joined output reads naturally. Does NOT handle abbreviations (`Dr. Smith` splits); documented limitation — conversational data rarely has those.

```rust
fn split_sentences(s: &str) -> Vec<&str>;
```

### 4.3 Scoring formula

```
score(sentence, i, N, query_tokens)
  = 0.7 * position_weight(i, N)
  + 0.3 * jaccard(sentence_tokens, query_tokens)

position_weight(i, N):
  i == 0        → 1.0   (topic sentence)
  i == N-1 AND N >= 3 → 0.8  (conclusion)
  otherwise     → 0.5   (middle)

jaccard(S, Q):
  if |S ∩ Q| == 0 OR |S ∪ Q| == 0 → 0.0
  else                             → |S ∩ Q| / |S ∪ Q|
```

### 4.4 Tokenization + stopwords

Tokenization: split on whitespace and punctuation, lowercase, no stemming.

Stopwords: hardcoded `const STOPWORDS: &[&str] = &[...]` with ~30 English function words (`a/an/the/is/was/are/were/i/you/it/and/or/but/to/of/in/on/for/with/as/at/by/this/that/these/those/have/has/had/do/did/not`). English-only; future phases can extend for multilingual retrieval.

### 4.5 SKIP rules

A hit passes through unchanged (no sentence scoring, no mutation) when any of:

- Sentence count < `COMPRESS_MIN_SENTENCES` (4)
- `snippet.len()` < `COMPRESS_MIN_CHARS` (400)

This bypass is important for layer=2 span hits, which are already 1-2 sentence extractive spans — running them through the compressor would over-prune or no-op, and the SKIP rule makes that clear at a glance.

### 4.6 Selection algorithm

```rust
pub fn compress_hits(hits, query, target_chars_per_hit) -> Vec<ResolvedHit> {
    let query_tokens = tokenize_query(query);
    hits.into_iter().map(|h| {
        let sentences = split_sentences(&h.snippet);
        if sentences.len() < COMPRESS_MIN_SENTENCES
           || h.snippet.len() < COMPRESS_MIN_CHARS {
            return h;  // SKIP: pass through unchanged
        }
        let scored: Vec<(usize, f64)> = sentences.iter().enumerate()
            .map(|(i, s)| (i, score_sentence(s, i, sentences.len(), &query_tokens)))
            .collect();
        let kept_indices = pick_by_score(&scored, &sentences, target_chars_per_hit);
        let kept_sorted = {
            let mut k = kept_indices;
            k.sort();
            k
        };
        let new_snippet = kept_sorted.iter()
            .map(|&i| sentences[i])
            .collect::<Vec<_>>()
            .join(" ");
        ResolvedHit { snippet: new_snippet, ..h }
    }).collect()
}
```

Kept sentences are emitted in **original document order** (sort by index, not score) so the compressed snippet reads naturally. The floor of 1 sentence per hit is enforced inside `pick_by_score`.

## 5. Prompt render integration (`ask/prompt.rs`)

### 5.1 Signature change

`prompt::render` gains a sixth positional argument:

```rust
pub fn render(
    question: &str,
    prior_turns: &[super::session::TurnRecord],
    hits: &[ResolvedHit],
    max_context_tokens: usize,
    response_tokens: usize,
    compress_enabled: bool,     // NEW
) -> RenderedPrompt
```

`ask_stream` reads `req.compress_enabled` from the new `AskRequest` field. Existing tests inside `prompt.rs` update with `true` (matches production default).

### 5.2 Three-stage overflow loop

```rust
let mut cur_tokens = tokens_est;
let mut history_cursor = 0usize;
let mut trimmed_hits = hits.len();
let mut compressed: Option<Vec<ResolvedHit>> = None;

// Stage 1 — compress once, only if over budget and enabled
if cur_tokens > max_context_tokens && compress_enabled {
    let overage_chars = (cur_tokens.saturating_sub(max_context_tokens)) * 4;
    let total_chars: usize = hits.iter().map(|h| h.snippet.len()).sum();
    let ratio = 1.0 - (overage_chars as f64 / total_chars.max(1) as f64).min(0.6);
    let avg = total_chars / hits.len().max(1);
    let target = (avg as f64 * ratio) as usize;
    compressed = Some(compress::compress_hits(hits.to_vec(), question, target));
    // Re-render user + recompute cur_tokens using compressed hits
    (user, valid_citations, ctx) = render_ctx_and_user(
        compressed.as_deref().unwrap(),
        &history_block,
        &truncated_question,
        trimmed_hits,
    );
    cur_tokens = (system.len() + user.len()) / 4 + response_tokens + 120;
}

// Stage 2 — existing: drop oldest history turn
while cur_tokens > max_context_tokens && history_cursor < prior_turns.len() {
    history_cursor += 1;
    // ... recompute history_block, re-render user, recompute cur_tokens
}

// Stage 3 — existing: shrink hits from tail
while cur_tokens > max_context_tokens && trimmed_hits > 1 {
    trimmed_hits -= 1;
    // ... re-render
}
```

### 5.3 Helper-fn extraction

Current `prompt::render` body has the ctx + user string construction inlined and duplicated across the two overflow loops. Phase 3.4's Stage 1 adds a third call site. Refactor: extract into a private helper:

```rust
fn render_ctx_and_user(
    hits: &[ResolvedHit],
    history_block: &str,
    truncated_question: &str,
    trimmed_hits: usize,
) -> (String /* user */, Vec<String> /* valid_citations */, String /* ctx */)
```

All three stages (initial render, Stage 1 post-compress re-render, Stage 3 hit-shrinking) call this helper, eliminating duplicated loop bodies and keeping the refactor's line count net-neutral.

### 5.4 Invariants preserved

- **Under-budget queries:** `cur_tokens <= max_context_tokens` on first render → Stage 1 skipped → `compressed` stays `None` → output byte-identical to Phase 3.3. Zero regression surface.
- **Compression disabled** (`compress_enabled=false`): Stage 1 skipped even under overflow → behaves exactly like Phase 3.3.
- **Citation count:** Stage 1 doesn't drop hits — it only shrinks snippets. `valid_citations.len()` is unchanged unless Stage 3 shrinks hits later.
- **Hit ordering:** Compression preserves input order. Citations `[1], [2], [3]` still align 1:1 with input hits.

### 5.5 Target-chars-per-hit derivation

The target budget per hit is derived from the overage:

```
overage_chars = (cur_tokens - max_context_tokens) * 4
total_chars   = sum of hit.snippet.len()
ratio         = 1.0 - min(overage_chars / total_chars, 0.6)
target        = avg_hit_chars * ratio
```

Cap at 60% reduction — compression is limited to keep >40% of each hit's content. If 60% reduction isn't enough to fit, Stage 2 (drop history) and Stage 3 (drop hits) handle the rest. This prevents the heuristic from over-pruning when budget is extremely tight.

## 6. Configuration

Single new field in `AskConfig` (`mur-common/src/config.rs`):

```rust
#[serde(default = "ask_default_compress_hits_enabled")]
pub compress_hits_enabled: bool,

fn ask_default_compress_hits_enabled() -> bool { true }
```

Plumbed through `ConversationsConfig.ask` (existing), threaded into `AskRequest` by `cmd_ask`:

```rust
// AskRequest gains:
pub compress_enabled: bool,
```

No commander sync extension — session-level config, not daemon-consumed.

No CLI flag — if a user needs to disable compression for diagnostics, they set `conversations.ask.compress_hits_enabled: false` in `config.yaml`. Flat rare-case.

## 7. Testing

### 7.1 Unit tests in `compress.rs` (6 tests)

1. `split_sentences_basic` — `"A. B! C?"` returns three non-empty items.
2. `jaccard_overlap_empty_query_is_zero` — guards divide-by-zero.
3. `position_weight_is_exact_constants` — first=1.0, last=0.8 (N≥3), middle=0.5.
4. `compress_hits_skips_short_hits` — hit with 2 sentences passes through unchanged.
5. `compress_hits_keeps_at_least_one_sentence` — floor invariant even with target_chars=0.
6. `compress_hits_preserves_citation_metadata` — `info`, `layer`, `line_hint`, `span_index_in_summary`, `vector` unchanged; only `snippet` shrinks.

### 7.2 Unit tests in `prompt.rs` (2 new tests)

7. `render_compresses_hits_on_overflow_when_enabled` — tight `max_context_tokens`, long hits, `compress_enabled=true` → output's hit snippets shorter than input AND history preserved.
8. `render_does_not_compress_when_disabled` — same inputs, `compress_enabled=false` → behavior matches Phase 3.3 overflow (drops history, hits unchanged).

All existing `prompt::render` tests (Phase 3.3) updated to pass `true` as the new 6th arg. Behavior unchanged on under-budget tests.

### 7.3 Integration test in `cli_conversations.rs` (1 test)

9. `mur_ask_compresses_long_hits_under_tight_budget` — writes a `config.yaml` with `max_context_tokens: 1000`, seeds long summary snippets via a direct-upsert path, runs `mur ask --json`, asserts the `answer` references hit content AND the session JSONL records the turn (proves compression ran without breaking citation resolution).

### 7.4 Golden path

No update. Phase 3.3's 17-step golden path covers the Ask pipeline on default-budget queries where compression doesn't fire. Phase 3.4 is a quality-of-life improvement on tight-budget queries, covered by the unit + integration tests above.

## 8. Success criteria

All of the following true at merge:

- `cargo test --workspace` green (existing + ~9 new tests).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo fmt --check` clean.
- `./scripts/golden-path-conversations.sh` still prints `=== ALL 17 STEPS GREEN ===` — no regression on existing assertions.
- `compress_hits` deterministic: same inputs produce byte-identical outputs across runs.
- Phase 3.3 under-budget prompt test fixtures (e.g., `render_includes_chat_history_section_when_prior_turns_non_empty`) pass unchanged after being updated with the 6th argument — proves zero regression on the happy path.

## 9. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Heuristic drops a sentence the LLM would have quoted | 60% max-reduction cap + floor of 1 sentence/hit + `compress_hits_enabled=false` escape hatch. |
| Sentence splitter breaks on non-English / heavy code | SKIP rule for short hits mitigates common cases; disable flag is the escape valve; documented as English-only limitation for §3 of the spec. |
| `target_chars_per_hit` computation is off and under-shrinks | Stage 2 (drop history) + Stage 3 (drop hits) still run after Stage 1; compression is additive safety, not a replacement. |
| Citation anchors break if hit is fully dropped | Stage 1 doesn't drop hits — only trims snippets. Floor of 1 sentence/hit preserves citation anchor content. |
| Performance regression (compression adds latency) | Under-budget queries skip Stage 1 entirely. Over-budget queries pay ~1-5ms CPU for sentence scoring across all hits — negligible vs Ollama generation time. |

## 10. References

- Phase 3.3 design spec: `docs/superpowers/specs/2026-04-21-mur-conversations-phase-3-3-design.md`
- Phase 3.2 design spec: `docs/superpowers/specs/2026-04-21-mur-conversations-phase-3-2-design.md`
- Chroma "Context Rot" research (2025) — hits matter more than distant history for RAG accuracy; backs the Stage 2 > Stage 3 ordering.
- Edmundson (1969) — position + keyword extractive summarization baseline.
- LexRank (2004), TextRank (2004) — graph-centrality extractive summarization (explicitly deferred per §2).
- LLMLingua-2 (Microsoft, EMNLP'23) — target architecture for future Phase 3.5+ upgrade.
- Mem0 chat-history summarization guide (Oct 2025) — rolling window + summarize older; informed Phase 3.3 window design.

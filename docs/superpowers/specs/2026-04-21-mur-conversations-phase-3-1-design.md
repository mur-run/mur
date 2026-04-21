# mur Conversations Phase 3.1 — RAPTOR-lite design

**Status:** Draft — spec under review.
**Depends on:** Phase 2A/2B/2C shipped (`a31690d`).
**Deferred to later phases:** Full RAPTOR tree (3.2), multi-turn chat (3.3), LLMLingua-2 (3.4), dashboard UI (3.5), A-MEM re-linking (3.6).

---

## 1. Purpose

Mode C (`mur ask`) today has two known retrieval-quality limitations, both tagged as Phase 3 concerns in the Phase 2 spec:

1. `ask::retrieve::resolve_summary_hit` always picks the **first** extractive span from a retrieved summary as the snippet, regardless of which span matches the query. If the query matches span 7, the citation still points at span 1.
2. `ask::retrieve::mmr_dedupe` uses word-Jaccard similarity to detect duplicates. Paraphrases of the same fact survive dedupe, polluting the final context with near-redundant snippets.

Phase 3.1 fixes both with minimal, additive changes: embed each extractive span at compact time and store it at a new LanceDB layer; retrieve at the span level directly; use cosine similarity on the retrieved vectors for MMR.

## 2. Non-goals

The following are **explicitly out of scope** for Phase 3.1 (deferred to later Phase 3 sub-projects):

- Week/month hierarchical summaries (Phase 3.2 — Full RAPTOR tree).
- Classic Carbonell–Goldstein MMR with tunable λ parameter.
- Query decomposition, HyDE-style query rewriting, cross-encoder re-ranking.
- Multi-turn follow-up questions (`mur ask --continue` — Phase 3.3).
- LLMLingua-2 prompt compression (Phase 3.4).
- Dashboard UI surfacing the new span layer (Phase 3.5).
- A-MEM dynamic re-linking on pattern change (Phase 3.6).

Each has its own spec cycle.

## 3. Architecture

Phase 3.1 is **additive**. No LanceDB schema migration, no on-disk layout change, no breaking change to `AskRequest`/`AskResponse`. The shape:

```
compact_day (writer):
  [Phase 2 stages unchanged: chunk → extract → abstract → macro → render .md → audit]
  +  embed each ExtractiveSpan.text (batched /api/embeddings call)
  +  upsert_with_layer at layer=2, one row per span:
       id       = "<src>_<conv>_L2_<line_hint>"
       ts       = date 00:00:00 UTC (summary date — source message ts not
                  available at reindex time via ParsedSpan; keep compact and
                  reindex paths symmetric)
       source   = span.src               (real source, not aggregated)
       conv_id  = span.conv_id           (real conv, not "summary:<date>")
       role     = Role::User              (placeholder — ignored at retrieval)
       layer    = 2
       content  = span.text
       vector   = Ollama embedding of span.text
       (Message.meta is not persisted to LanceDB — span_index is recovered
        from the id suffix at ask time.)

  (layer=1 narrative row still written as before — backward compat)

ask::retrieve::gather_hits:
  query_embed
  → idx.search(q, k_summary, src, layer=Some(2))    // preferred
      if hits.is_empty():
        idx.search(q, k_summary, src, layer=Some(1)) // fallback (unmigrated)
        → resolve_summary_hit()                      // Phase 2 path
  → each SearchHit now carries layer + vector
  → mmr_dedupe_cosine: cosine(a.vector, b.vector) > mmr_threshold
  → cap_by_budget (unchanged)
  → [prompt/generate/cite/format stages unchanged]
```

**Invariants preserved:**
- `layer=0` (raw) untouched.
- `layer=1` (narrative) rows still produced every compact; serve as fallback for unmigrated archives.
- Citation anchor format unchanged: `[cit: <date> <src>/<conv> @summary-span-<N>]`. At layer=2, N comes from the id suffix; at layer=1, N still comes from `resolve_summary_hit`'s first-span fallback.
- No LanceDB column change. Existing rows remain readable.

## 4. Compact-side changes

**Files:**
- `mur-common/src/conversation.rs` — add `Source::from_prefix(&str) -> Option<Self>` helper (mirror of `file_prefix()`).
- `mur-core/src/conversations/summarize/mod.rs` — orchestrator batch-embeds spans, threads them into the writer call.
- `mur-core/src/conversations/summarize/writer.rs` — `write_summary` signature gains `span_embeddings`; internally upserts N layer=2 rows after the existing layer=1 upsert.
- `mur-core/src/conversations/index.rs` — `upsert_internal` id pattern extends to `<prefix>_<conv>_L<layer>_<N>` so layer=2 rows don't collide with layer=0.

### Orchestrator (`summarize::mod::compact_day`)

After computing `summary_embedding` for the narrative, batch-embed the spans:

```rust
let span_embeddings: Vec<Vec<f32>> = if OllamaClient::mock_from_env() {
    doc.extractive.iter().map(|_| vec![0.1; embed_dims]).collect()
} else {
    let texts: Vec<String> = doc.extractive.iter().map(|s| s.text.clone()).collect();
    match load_config() {
        Ok(cfg) => embed_batch(&texts, &EmbeddingConfig::from_config(&cfg))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("span embedding failed, falling back to zero vectors: {e:#}");
                texts.iter().map(|_| vec![0.0; embed_dims]).collect()
            }),
        Err(_) => texts.iter().map(|_| vec![0.0; embed_dims]).collect(),
    }
};
writer::write_summary(&doc, summary_embedding, span_embeddings, root_override).await?;
```

Zero-vector fallback on embedding failure keeps the compact run committing the summary; `mur conversations reindex --spans-only` can repair the index later.

### Writer (`summarize::writer::write_summary`)

New signature:

```rust
pub async fn write_summary(
    doc: &SummaryDoc,
    summary_embedding: Vec<f32>,
    span_embeddings: Vec<Vec<f32>>,     // NEW — one per doc.extractive, same order
    root_override: Option<&str>,
) -> Result<WriteResult>
```

After the existing layer=1 narrative upsert, upsert one row per extractive span:

```rust
if !doc.extractive.is_empty() && doc.extractive.len() == span_embeddings.len() {
    let mut batch = Vec::with_capacity(doc.extractive.len());
    for (span, vec) in doc.extractive.iter().zip(span_embeddings.into_iter()) {
        let msg = mur_common::Message {
            v: 1,
            ts: chrono::Utc.from_utc_datetime(&doc.date.and_hms_opt(0, 0, 0).unwrap()),
            src: span.src,
            conv: span.conv_id.clone(),
            role: mur_common::Role::User,
            content: mur_common::Content::Text { value: span.text.clone() },
            meta: serde_json::Value::Null,
            refs: vec![],
        };
        batch.push((msg, vec, 2i8));
    }
    idx.upsert_with_layer(&batch).await?;
}
```

### Id uniqueness

`upsert_internal` currently builds ids as `<prefix>_<conv>_<i>` where `i` is the batch index. For a narrative batch of size 1, id is `<prefix>_summary:<date>_0`. For a span batch, collisions with layer=0 raw rows (same prefix, same conv, same batch index) are theoretically possible.

Extend the id builder to include the layer when != 0:

```rust
let ids: Vec<String> = entries.iter().enumerate().map(|(i, (m, _, layer))| {
    if *layer == 0 {
        format!("{}_{}_{}", m.src.file_prefix(), m.conv, i)
    } else {
        // For layer=2 span rows, use the line_hint (stored in the upsert batch as `i`
        // only when the caller passes spans in line-hint order). Callers should pass
        // (message, vector, layer) triples where batch index == desired suffix.
        format!("{}_{}_L{}_{}", m.src.file_prefix(), m.conv, layer, i)
    }
}).collect();
```

The writer passes spans in line-hint order: the batch index `i` corresponds to the span's position in `doc.extractive` which is already sorted by `line_hint`. For citation decoding, the caller recovers the span_index via the id suffix. This keeps backward compat — layer=0 ids are unchanged.

## 5. Ask-side changes

**Files:**
- `mur-core/src/conversations/index.rs` — extend `SearchHit` + include `vector` + `layer` columns in search results.
- `mur-core/src/conversations/ask/retrieve.rs` — new search strategy, new `resolve_span_hit`, cosine-based MMR.
- `mur-common/src/config.rs` — retune `mmr_threshold` default.

### `SearchHit` extension

```rust
pub struct SearchHit {
    pub id: String,
    pub ts: i64,
    pub source: Source,
    pub conv_id: String,
    pub content: String,
    pub distance: f32,
    pub layer: i8,                    // NEW
    pub vector: Option<Vec<f32>>,     // NEW
}
```

In `ConversationIndex::search`, extend `.select_columns(&[...])` to include `"layer"` and `"vector"` explicitly. Decode the vector from the returned `FixedSizeListArray`. Storage overhead per search: k hits × 4 kB ≈ 40 kB for k=10; negligible for local LanceDB.

### `ResolvedHit` extension

```rust
pub struct ResolvedHit {
    pub layer: i8,
    pub info: HitInfo,
    pub snippet: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub vector: Option<Vec<f32>>,     // NEW — carried forward from SearchHit
}
```

### `gather_hits` strategy

```rust
let l2 = idx.search(&query_embedding, k_summary, primary_src, Some(2)).await?;
let l1 = if l2.is_empty() {
    idx.search(&query_embedding, k_summary, primary_src, Some(1)).await?
} else { Vec::new() };

let effective_top = l2.first().map(similarity_of)
    .or_else(|| l1.first().map(similarity_of))
    .unwrap_or(0.0);
let l0 = if !no_escalate
    && (effective_top < threshold || (l2.is_empty() && l1.is_empty()))
{
    idx.search(&query_embedding, k_raw, primary_src, Some(0)).await?
} else { Vec::new() };

let mut resolved = Vec::new();
for h in l2.into_iter().filter(|h| passes(h, filters)) { resolved.push(resolve_span_hit(h)?); }
for h in l1.into_iter().filter(|h| passes(h, filters)) { resolved.push(resolve_summary_hit(h, root_override)?); }
for h in l0.into_iter().filter(|h| passes(h, filters)) { resolved.push(resolve_raw_hit(h)); }

let deduped = mmr_dedupe_cosine(resolved, mmr_threshold);
cap_by_budget(deduped, budget)
```

### `resolve_span_hit`

Span-layer resolution is trivial — the hit IS the snippet:

```rust
fn resolve_span_hit(h: SearchHit) -> Result<ResolvedHit> {
    let line_hint = h.id.rsplit_once("_L2_")
        .and_then(|(_, suffix)| suffix.parse::<u32>().ok());
    Ok(ResolvedHit {
        layer: 2,
        info: HitInfo {
            layer: 2,
            source: h.source.file_prefix().to_string(),
            conv_id: h.conv_id.clone(),
            date: date_from_ts(h.ts),
            score: similarity_of(&h),
        },
        snippet: h.content.clone(),
        line_hint,
        span_index_in_summary: line_hint,
        vector: h.vector,
    })
}
```

### `mmr_dedupe_cosine`

Replace the word-Jaccard check with a cosine-similarity check on the retrieved vectors:

```rust
fn mmr_dedupe_cosine(hits: Vec<ResolvedHit>, threshold: f64) -> Vec<ResolvedHit> {
    let mut kept: Vec<ResolvedHit> = Vec::new();
    for h in hits {
        let dup = kept.iter().any(|k| similar(&h, k, threshold));
        if !dup { kept.push(h); }
    }
    kept
}

fn similar(a: &ResolvedHit, b: &ResolvedHit, threshold: f64) -> bool {
    match (&a.vector, &b.vector) {
        (Some(av), Some(bv)) => cosine_sim(av, bv) > threshold,
        _ => word_jaccard(&a.snippet, &b.snippet) > threshold,
    }
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let mut dot = 0.0f64; let mut na = 0.0f64; let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (x * y) as f64;
        na += (x * x) as f64;
        nb += (y * y) as f64;
    }
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na.sqrt() * nb.sqrt()) }
}
```

The mixed-fallback path (one hit has a vector, the other doesn't — possible when ask pulls both layer=2 and layer=1 hits on the escalation path) uses word-Jaccard as a best-effort proxy. In practice this is rare since layer=2 empties early-out the fallback to layer=1.

### Config default change

`mmr_threshold` default shifts from **0.85** (word-Jaccard scale) to **0.88** (cosine scale) in `mur-common/src/config.rs`. Users with explicit overrides keep their value — cosine interpretation is approximately monotonic enough that existing tuning mostly still holds.

## 6. Migration

### CLI surface

`mur conversations reindex` expands to handle both layers. Two new flags preserve narrower workflows:

| Invocation | Behavior |
|---|---|
| `mur conversations reindex` | Full rebuild: raw → layer=0, summaries → layer=2. Default. |
| `mur conversations reindex --raw-only` | Preserves Phase 2 behavior (layer=0 only). |
| `mur conversations reindex --spans-only` | Only rebuilds layer=2 from existing `summary/*.md`. Fastest upgrade path. |

Implementation:
1. If `!raw_only`: walk `~/.mur/conversations/summary/*.md`, parse each via `summarize::parse_summary`, batch-embed per-summary, upsert layer=2 rows with id `<prefix>_<conv>_L2_<line_hint>`. Use `Source::from_prefix(&span.src)` to convert the parsed prefix string back to `Source`.
2. If `!spans_only`: existing raw-rebuild path runs unchanged.
3. Print per-section progress: `reindexed 134 raw days (8,923 messages); reindexed 134 summaries (2,781 spans)`.

### Doctor update

Extend `cmd_conversations_doctor` output with span coverage:

```
  ✓ spans: 8,432 rows at layer=2
```

Or, if no layer=2 rows present but summaries exist:

```
  · spans: 0 indexed — run 'mur conversations reindex --spans-only' for span-level Ask retrieval
```

### First-ask UX for unmigrated users

Silent fallback. `mur ask` detects `l2.is_empty()` and proceeds on the layer=1 narrative path (Phase 2 behavior). Doctor is the nudge; printing a warning on every ask call would be noise.

### Idempotency

Reindex is safe to run repeatedly. LanceDB upserts by id (content-addressed); identical ids overwrite cleanly. No explicit delete pass needed.

## 7. Config schema

No new config keys. One default value changes:

```rust
// mur-common/src/config.rs
fn default_mmr_threshold() -> f64 {
    0.88    // was 0.85 — cosine-interpreted in Phase 3.1
}
```

The `mur_common::config::AskConfig::mmr_threshold` field name and semantics survive; only the default shifts. Users with explicit `0.85` in their config retain it — cosine at 0.85 is slightly more permissive than Jaccard at 0.85 was, but still catches obvious duplicates.

## 8. Testing

### Unit tests (no Ollama, no I/O)

- `cosine_sim` — identical → 1.0; orthogonal → 0.0; zero-length inputs → 0.0 (no NaN); mismatched length → 0.0.
- `resolve_span_hit` — id `"cc_abc_L2_17"` → `line_hint = Some(17)`; id without `_L2_` suffix → `line_hint = None`.
- `mmr_dedupe_cosine` — two near-identical vectors drop to one; diverse pair both kept; mixed vector/no-vector pair falls back to word-Jaccard; empty input → empty output.
- `Source::from_prefix` — seven known prefixes round-trip; unknown prefix → `None`.
- `id_for_span_row` helper — given (src, conv, line_hint) produces stable `<prefix>_<conv>_L2_<line>`.

### Writer unit tests

- `write_summary_upserts_span_rows` — after write, `idx.search(..., Some(2))` returns N hits where N = `doc.extractive.len()`.
- `write_summary_with_empty_spans_writes_no_layer_2` — edge case where compact produced zero quote-worthy spans.
- `write_summary_span_ids_are_stable` — two writes of the same date produce span rows with identical ids (idempotent via upsert).

### `ask::retrieve` unit tests

- `gather_hits_prefers_layer_2` — seed both layer=1 and layer=2 rows; assert all returned hits are layer=2.
- `gather_hits_falls_back_to_layer_1_when_no_spans` — seed only layer=1; assert Phase 2B backward-compat path fires.
- `gather_hits_preserves_escalation_to_layer_0` — if layer=2 top score below threshold, layer=0 raw hits appear in the result set.

### Reindex tests

- `reindex_spans_only_on_seeded_archive_populates_layer_2` — seed `summary/*.md`, run `cmd_conversations_reindex` with `spans_only=true`, assert `idx.count_rows_at_layer(2) > 0`.
- `reindex_raw_only_does_not_touch_layer_2` — pre-populate layer=2, run with `raw_only=true`, assert layer=2 row count unchanged.

### CLI integration tests (`tests/cli_conversations.rs`)

- `mur_conversations_reindex_spans_only_runs` — seed a summary, invoke `mur conversations reindex --spans-only`, assert exit 0 + stdout contains `"reindexed"` + `"spans"`.
- Extend `mur_conversations_doctor_runs` assertion to include `"spans:"` substring check.

### Deterministic-hash mock for embeddings

The existing `MUR_OLLAMA_MOCK=1` returns `vec![0.1; 1024]` for every input — fine for tests that just exercise code paths, but useless for asserting span-selection picks the right span (all vectors are identical, cosine = 1.0). Phase 3.1 adds a second mock mode `MUR_OLLAMA_MOCK=hash`: seed a 1024-dim vector from `sha256(text)[..32]` expanded via a simple PRNG, L2-normalize. Deterministic, text-distinguishing, sub-microsecond per call. Gated to preserve the existing `=1` behavior for tests that don't care about distinct vectors.

### Golden-path update

Extend `scripts/golden-path-conversations.sh` after Step 9 (compact):

- Step 9.5: `MUR_OLLAMA_MOCK=hash mur conversations reindex --spans-only` — assert exit 0.
- Step 10 (existing ask): re-run with `MUR_OLLAMA_MOCK=hash`. Assert the JSON response's first citation's `span_index_in_summary` matches the span whose text shares the highest token overlap with the question — deterministic under hash-mock.

### Real-Ollama smoke

No new smoke test required. The Phase 2C `ollama-live-smoke` test already exercises `compact_day` end-to-end; with span-upsert added to the writer, it covers the new path automatically.

## 9. Success criteria

Phase 3.1 ships when, on a seeded archive:

- A day with 10 extractive spans, query matching the 7th span's text → `mur ask --json` returns a citation with `span_index_in_summary == 7` (not `1`). Verified with hash-mock.
- Two conversations whose spans share near-identical text → after MMR, only one survives in `hits_used`. Verified with hash-mock.
- Archives that haven't been reindexed continue to answer correctly via the layer=1 fallback; all Phase 2B CLI integration tests pass unchanged.
- `mur conversations reindex --spans-only` on a 30-day archive completes in < 30 s under `MUR_OLLAMA_MOCK=hash`.

## 10. Risks & open questions

**Risks:**

- **Ollama embedding latency at compact time.** Phase 2 ran one embedding per day (the narrative). Phase 3.1 adds ~20 embeddings per day (spans), batched into one `/api/embeddings` call per compact run. Latency increase: ~1-3 s per day. Acceptable for a nightly job; if a user backfills 180 days via `reindex`, that's ~3-9 minutes — acceptable for a one-time upgrade action.
- **LanceDB row-count scaling.** At ~20 spans/day, 365 days = 7,300 span rows/year. LanceDB handles millions without issue; no concern.
- **Cosine threshold tuning.** The 0.85 → 0.88 default change could drop hits that should stay (if cosine scale is less permissive than expected) or keep hits that should drop (if it's more permissive). Mitigation: the Jaccard fallback path inside `similar()` provides a safety net, and users can override via `config.yaml`.

**Open questions:** None blocking implementation.

## 11. References

- Phase 2 design: `docs/superpowers/specs/2026-04-20-mur-conversations-phase-2-design.md`
- Phase 2 plan: `docs/superpowers/plans/2026-04-20-mur-conversations-phase-2.md`
- Phase 2 ship commits: `8f067cf` (2A), `fefe6fb` (2B), `a31690d` (2C).
- RAPTOR paper (Sarthi et al., 2024) — `https://arxiv.org/abs/2401.18059` — inspiration for the span-level retrieval unit.
- MMR original: Carbonell & Goldstein, 1998 — "The Use of MMR, Diversity-Based Reranking for Reordering Documents" — referenced but not implemented in 3.1; reserved for 3.2 if diversity tuning becomes a need.

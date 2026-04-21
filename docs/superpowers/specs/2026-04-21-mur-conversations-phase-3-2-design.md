# mur Conversations Phase 3.2 — Full RAPTOR tree design

**Status:** Draft — spec under review.
**Depends on:** Phase 3.1 shipped (`ed7f883`).
**Deferred to later phases:** multi-turn `--continue` (3.3), LLMLingua-2 (3.4), dashboard UI (3.5), A-MEM re-linking (3.6), quarterly/yearly layers (3.2.1 if wanted).

---

## 1. Purpose

Mode C (`mur ask`) currently surfaces day summaries (`layer=1`), per-span rows (`layer=2`, Phase 3.1), and raw messages (`layer=0`). Long-time-window queries — "what did I work on in March?", "summarize last week" — still pull 30+ day summaries into the prompt and overflow the token budget, forcing `cap_by_budget` to drop the most relevant content.

Phase 3.2 adds two new summary layers on top of the existing tree:
- `layer=3` — weekly rollup (ISO week, Mon–Sun).
- `layer=4` — monthly rollup (calendar month).

Rollups stay grounded via a **hybrid pipeline** — extractive spans come from the union of the window's day-layer spans (MMR-deduped), abstractive narrative is one LLM call over those spans plus the source narratives as framing context. Retrieval becomes a **collapsed-tree search** across layers 1/2/3/4 in one k-NN pool per layer, merged + MMR-deduped — the embedding model picks the right abstraction via cosine similarity rather than hardcoded tiering.

## 2. Non-goals

Explicitly out of scope for Phase 3.2:

- Quarterly or yearly rollups — not enough user need yet; add as 3.2.1 if requested.
- Clustering-based RAPTOR (the paper's recursive tree). Single-user archive doesn't have the diversity to warrant clustering; hybrid MMR is sufficient.
- Multi-turn `mur ask --continue` (Phase 3.3).
- LLMLingua-2 prompt compression (Phase 3.4).
- Dashboard UI surfacing rollup layers (Phase 3.5).
- Query decomposition / HyDE / time-aware query routing — Q3a collapsed tree handles time-scope via embedding.
- Cross-encoder rerank.
- Tunable λ-MMR (current: threshold).

## 3. Architecture

Phase 3.2 is **additive**. No LanceDB schema migration. New paths, new layers, new CLI subcommand. The compact trigger in commander does not change — daily tick fires `mur conversations compact` which cascades into the rollup sweep.

```
daily trigger (commander, unchanged) → `mur conversations compact`
  │
  ├─ Phase 2A: compact_missing over raw/* → day summaries (layer=1) + layer=2 spans
  │
  └─ Phase 3.2 (new): rollup_missing — scans for missing weekly + monthly rollups
      │
      ├─ rollup_week: reads 7 day summaries, gathers cross-day layer=2 spans via
      │              idx.scan_rows_at_layer(2, ts_lo, ts_hi), MMR-deduplicates,
      │              truncates to max_extractive_spans_per_week, runs one
      │              abstractive LLM call with (selected spans + 7 day narratives
      │              as framing), renders and writes summary/weekly/<YYYY-Wnn>.md,
      │              upserts a single layer=3 row to LanceDB.
      │
      └─ rollup_month: reads 4–5 weekly summaries, same pattern; writes
                      summary/monthly/<YYYY-MM>.md, upserts layer=4.

ask::retrieve::gather_hits (collapsed tree):
  query_embed
  → parallel searches, one per layer: layer=2, layer=1, layer=3, layer=4.
    Each returns ceil(k_summary / 4) hits. Layer=0 reached only via escalation
    when all upper layers are empty or top score is below escalation_threshold.
  → merge all hits into one pool
  → resolve layer-specifically: resolve_span_hit | resolve_summary_hit |
    resolve_week_hit | resolve_month_hit | resolve_raw_hit
  → sort by similarity score (mixed-layer ranking)
  → mmr_dedupe_cosine across the whole pool (drops cross-layer duplicates)
  → cap_by_budget
```

**Invariants preserved:**

- `layer=0/1/2` rows, row ids, and file paths untouched.
- Existing `summary/<YYYY-MM-DD>.md` path unchanged.
- Phase 2 citation anchors unchanged: `[cit: <date> <src>/<conv> @summary-span-<N>]`, etc. New anchors for layer=3/4: `[cit: <YYYY-Wnn> week/<YYYY-Wnn>]`, `[cit: <YYYY-MM> month/<YYYY-MM>]`.
- `ConversationsCompactTrigger` in commander's daemon unchanged.
- Phase 3.1's `layer=2 → layer=1` empty-fallback is replaced by the collapsed-tree escalation to layer=0 when ALL upper layers are empty — covers the unmigrated-archive case the same way, just broader.

**New files:**

- `mur-core/src/conversations/summarize/rollup.rs` — `rollup_week`, `rollup_month`, `rollup_missing`, window-label parsing (`iso_week_bounds`, `iso_week_monday`, `month_first_day`).

**Modified files (roughly):**

- `mur-common/src/config.rs` — new `RollupConfig`.
- `mur-core/src/conversations/paths.rs` — `weekly_summary_*`, `monthly_summary_*`, history dirs.
- `mur-core/src/conversations/audit.rs` — new `AuditAction::Rollup` variant.
- `mur-core/src/conversations/index.rs` — `scan_rows_at_layer(layer, ts_lo, ts_hi)` helper.
- `mur-core/src/conversations/summarize/{mod,writer,abstractive}.rs` — `write_rollup`, `rollup_abstractive_narrative`, new `RollupDoc` type.
- `mur-core/src/conversations/ask/retrieve.rs` — `gather_hits` rewritten as collapsed tree; add `resolve_week_hit`, `resolve_month_hit`.
- `mur-core/src/conversations/ask/prompt.rs` — `cite_anchor` extended for layer=3/4.
- `mur-core/src/cmd/conversations_cmd.rs` — new `cmd_conversations_rollup`; extend `cmd_conversations_compact` to cascade into rollup; extend `cmd_conversations_reindex` with `--rollups-only`; extend `cmd_conversations_doctor` with rollup coverage.
- `mur-core/src/main.rs` — add `ConversationsAction::Rollup` variant + reindex flag.
- `mur-core/src/conversations/migrate.rs` — extend P4 commander config sync with `[conversations.rollup]`.
- `mur-core/src/conversations/ollama.rs` — `mock_generate` branches on `"week"`/`"month"` substrings for distinct rollup content.
- `scripts/golden-path-conversations.sh` — Steps 11.5 / 12 / 13 / 14.

## 4. Rollup pipeline internals

### 4.1 Span selection for week rollup

```rust
pub async fn rollup_week(
    iso_week: &str,           // e.g. "2026-W16"
    force: bool,
    cfg: &RollupConfig,
    root_override: Option<&str>,
) -> Result<RollupReport> {
    let (monday, sunday) = iso_week_bounds(iso_week)?;
    let dates: Vec<NaiveDate> = (0..7).map(|i| monday + Duration::days(i)).collect();

    // Read available day summaries (tolerant of missing days).
    let day_narratives: Vec<(NaiveDate, String)> = dates.iter()
        .filter_map(|d| std::fs::read_to_string(summary_paths_for(*d, root_override).0).ok()
            .and_then(|body| parse_summary(&body).ok())
            .map(|p| (*d, p.narrative)))
        .collect();
    if day_narratives.is_empty() {
        return Ok(RollupReport::skipped(iso_week, "no source days"));
    }

    // Collect all layer=2 span rows in the week window, with their vectors.
    let idx = ConversationIndex::open(dims, root_override).await?;
    let ts_lo = monday.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    let ts_hi = (sunday + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    let span_rows: Vec<SearchHit> = idx.scan_rows_at_layer(2, ts_lo, ts_hi).await?;

    // MMR-deduplicate via existing cosine MMR.
    let hits: Vec<ResolvedHit> = span_rows.into_iter()
        .map(resolved_hit_from_span_row)
        .collect();
    let deduped = mmr_dedupe_cosine(hits, cfg.week_mmr_threshold);

    // Sort chronologically and truncate.
    let mut selected = deduped;
    selected.sort_by_key(|h| (h.info.date, h.line_hint.unwrap_or(0)));
    if selected.len() > cfg.max_extractive_spans_per_week as usize {
        selected.truncate(cfg.max_extractive_spans_per_week as usize);
    }

    // Abstractive pass (see 4.2).
    // Macro refs rewrite.
    // Render + write_rollup (see 4.3).
    // Upsert single layer=3 row.
    ...
}
```

`rollup_month` is structurally identical but reads weekly summaries instead of day summaries, gathers layer=2 spans across the month's ts range (same `scan_rows_at_layer(2, ...)` call — month-layer rollup still cites verbatim day-level spans, not aggregated week narratives), and writes `summary/monthly/<YYYY-MM>.md` + layer=4.

### 4.2 Abstractive prompt shape

New function in `summarize/abstractive.rs`:

```rust
pub struct RollupAbstractiveInput<'a> {
    pub kind: RollupKind,                                    // Week | Month
    pub window_label: &'a str,                               // "2026-W16" or "2026-04"
    pub selected_spans: &'a [ResolvedHit],                   // cross-day, MMR-deduped, truncated
    pub prior_narratives: &'a [(String, String)],            // [(date_or_week_label, narrative)]
}

pub async fn rollup_narrative(
    client: &OllamaClient,
    model: &str,
    input: &RollupAbstractiveInput<'_>,
    max_words: u32,
) -> AbstractiveResult;
```

Prompt structure:

```
System: You are summarizing one [week|month] ({window_label}) of a developer's
AI-assistant conversations into a narrative paragraph. Use ONLY information
present in the spans below. The prior narratives are context for framing —
do not quote them verbatim. Reference each key fact by its span index [N].

Output: {min_words}–{max_words} words, first-person or neutral, no bullet
lists. Do NOT invent details.

Spans (cross-day, deduplicated):
  [1] {2026-04-15 cc/abc L3}: "cargo build failed with E0001 during the
                               lance-upgrade merge"
  [2] {2026-04-16 cc/xyz L7}: "chose to pin tokio = 1.52 after the 1.53
                               regression"
  ...

Prior narratives (context only, do not quote):
  2026-04-15: ...
  2026-04-16: ...
  ...

Write the narrative.
```

Key differences from Phase 2A's day-level abstractive prompt:
1. Two input sources (spans + prior narratives).
2. Explicit instruction: quote via `[N]` only from spans; narratives are framing.
3. Larger word budget (defaults: 500 for week, 700 for month) since the window is larger.

Mock behavior: the existing `ollama::mock_generate` in `ollama.rs` branches on `"narrative paragraph"` which appears in the Phase 2A prompt. The rollup prompt includes the same phrase (system line: "...narrative paragraph..."), so the mock path routes correctly. For tests that need DISTINCT rollup content, extend the mock: branch additionally on `"Week"` / `"Month"` substrings in the prompt and return distinctive mock narratives (`"Mock narrative: this week..."` / `"Mock narrative: this month..."`).

### 4.3 Writer entry point

```rust
// summarize/writer.rs

pub enum RollupKind { Week, Month }

pub struct RollupDoc {
    pub kind: RollupKind,
    pub window_label: String,             // "2026-W16" or "2026-04"
    pub window_start: NaiveDate,          // Monday for week; 1st-of-month for month
    pub source_labels: Vec<String>,       // ["2026-04-13".."2026-04-19"] for week; ["2026-W14".."2026-W17"] for month
    pub generated_at: DateTime<Utc>,
    pub extractive_model: String,
    pub abstractive_model: String,
    pub mur_version: String,
    pub duration_ms: u64,
    pub sources: Vec<String>,             // union of src prefixes across spans
    pub pattern_refs: Vec<MacroRef>,
    pub keywords: Vec<String>,
    pub links_prev: Option<String>,
    pub links_next: Option<String>,
    pub warnings: Vec<String>,
    pub input_content_sha: String,
    pub extractive: Vec<ExtractiveSpan>,
    pub abstractive: AbstractiveResult,
}

pub async fn write_rollup(
    doc: &RollupDoc,
    narrative_embedding: Vec<f32>,
    root_override: Option<&str>,
) -> Result<WriteResult>
```

Body mirrors `write_summary` (Phase 2A). Differences:

- Output path resolves via `weekly_summary_path_for(window_label, root_override)` or `monthly_summary_path_for(window_label, root_override)`.
- `.history/` dir: `weekly_history_dir` / `monthly_history_dir`.
- Audit append: `AuditAction::Rollup { kind: "week" | "month", window: window_label, model, duration_ms }`.
- LanceDB upsert: one row at `layer=3` (week) or `layer=4` (month). Row shape:
  - `id` → `wk_<window>_L3_0` or `mo_<window>_L4_0` (the synthetic suffix `0` is fine — only one row per window).
  - `ts` → `window_start.and_hms_opt(0,0,0).unwrap().and_utc().timestamp()`.
  - `source` → synthetic `week` or `month` prefix. Added to `Source::from_prefix` (new variants? no — keep `Source` closed; use the `source` column string directly without routing through the enum. The row's `source` is stored as String in LanceDB regardless of enum; ingesting via `Message.src` doesn't work because `Source` doesn't have a `Week`/`Month` variant. Workaround: write rollup rows via a new `upsert_rollup_row` helper that sets the `source` column to `"week"` or `"month"` directly, bypassing the `Message` → `Source::file_prefix` path).
  - `conv` → `"week:<window>"` or `"month:<window>"`.
  - `role` → `User` (placeholder; ignored at retrieval).
  - `content` → the rollup narrative.
  - `vector` → passed-in narrative embedding.

### 4.4 Rollup rows via `upsert_rollup_row`

Adding `Week`/`Month` variants to `mur_common::Source` would be a breaking ripple (`file_prefix` match arms, `from_prefix` matches, existing tests expecting 8 variants). Instead, add a direct-write method to `ConversationIndex`:

```rust
pub struct RollupRow<'a> {
    pub id: &'a str,                     // fully-formed id
    pub ts: i64,
    pub source: &'a str,                 // "week" or "month" literal
    pub conv_id: &'a str,                // "week:<window>" or "month:<window>"
    pub layer: i8,                       // 3 or 4
    pub content: &'a str,
    pub vector: &'a [f32],
}

impl ConversationIndex {
    pub async fn upsert_rollup_row(&mut self, row: RollupRow<'_>) -> Result<()>;
}
```

Takes the raw column values — no `Message` intermediate. ID is pre-formed (writer constructs `wk_<window>_L3_0`). At retrieval time, `resolve_week_hit` / `resolve_month_hit` parse the synthetic conv_id prefix (`week:...`, `month:...`) to recover the window label — no `Source::from_prefix` lookup needed, since `h.source` on the SearchHit is a `Source` enum that must be constructed when decoding the row. Decoding fallback: if `h.source` string doesn't match a known prefix, map to a new internal-only `Source::__Rollup` variant? No — simpler: the SearchHit decode loop in `ConversationIndex::search` already has a `match srcs.value(i)` with a fallback `anyhow::bail!("unknown source tag")`. Relax the fallback: unknown source strings get mapped to `Source::ClaudeCode` as a placeholder (the caller uses `h.source` only for filtering, and rollup rows are filtered by layer, not by source). Document this.

### 4.5 Frontmatter schema (rollup `.md`)

Same shape as Phase 2A's day summary with three additions:

```yaml
---
schema: 1
kind: week                                # NEW — "week" | "month"
window: 2026-W16                          # NEW — label
source_labels: [2026-04-13, ..., 2026-04-19]  # NEW — what this rollup was generated from
date: 2026-04-13                          # reuses existing key — = window_start
generated_at: 2026-04-20T03:00:00Z
generated_by:
  extractive_model: qwen3:14b
  abstractive_model: qwen3:14b
  mur_version: 2.4.0
duration_ms: 2300
conv_count: 47                            # aggregate across source days
msg_count: 312                            # aggregate
sources: [cc, cursor, gemini]             # union
pattern_refs: []
keywords: [...]
links:
  prev: 2026-W15                          # window-label of previous window
  next: 2026-W17
warnings: []
input_content_sha: <sha256 of concatenated source shas>
---

## Extractive spans

[1] _{cc/abc @L3}_:
> cargo build failed with error E0001

...

## Abstractive narrative

This week (2026-W16) was dominated by the lance-upgrade merge...

## Macro expansion map

- {{pattern: rabitq-compression}} → patterns/rabitq-compression.yaml (v2, sha abcd1234…)
```

The `kind` and `window` frontmatter fields let `parse_summary` (Phase 2 Task 11) detect rollup files vs day files; the reader returns a `ParsedSummary` with a new optional `kind: Option<RollupKind>` field.

### 4.6 Idempotency via `input_content_sha`

- Week rollup's `input_content_sha` = `sha256(day_sha_0 || "\n" || day_sha_1 || ... || day_sha_6)`, where each `day_sha_i` is the `input_content_sha` of the corresponding day summary's frontmatter.
- Month rollup's sha = same pattern over the 4–5 constituent week summaries' shas.

When `rollup_week` is invoked with `force=false`, it computes the fresh sha, opens the existing `summary/weekly/<window>.md` (if any), extracts its frontmatter's `input_content_sha`, and skips if they match.

### 4.7 Error tolerance

- Missing day summaries in a week window → generate with what's present, add `"incomplete: missing N of 7 days"` to `warnings`.
- All days missing → return `Outcome::Skipped { reason: "no source days" }`. No file written.
- Missing weeks within a month → same pattern.
- Ollama failure during abstractive → `AbstractiveResult.narrative = None`; writer emits body with `"(rollup narrative generation failed; see warnings)"`. Layer=3/4 row still written with a zero vector so retention logic + doctor coverage reporting stay consistent. `mur conversations reindex --rollups-only` fixes later.

## 5. Retrieval integration

`gather_hits` rewritten as collapsed-tree search:

```rust
pub async fn gather_hits(args: RetrieveArgs<'_>) -> Result<Vec<ResolvedHit>> {
    let dims = args.query_embedding.len() as i32;
    let idx = ConversationIndex::open(dims, args.root_override).await?;
    let primary_src = args.filters.source.first().copied();

    let k_each = (args.k_summary as u32).div_ceil(4).max(1) as usize;
    let l2 = idx.search(&args.query_embedding, k_each, primary_src, Some(2)).await?;
    let l1 = idx.search(&args.query_embedding, k_each, primary_src, Some(1)).await?;
    let l3 = idx.search(&args.query_embedding, k_each, primary_src, Some(3)).await?;
    let l4 = idx.search(&args.query_embedding, k_each, primary_src, Some(4)).await?;

    let upper_empty = l2.is_empty() && l1.is_empty() && l3.is_empty() && l4.is_empty();
    let effective_top = [&l2, &l1, &l3, &l4].iter()
        .filter_map(|v| v.first()).map(similarity_of)
        .fold(0.0, f64::max);
    let l0 = if !args.no_escalate && (upper_empty || effective_top < args.escalation_threshold) {
        idx.search(&args.query_embedding, args.k_raw, primary_src, Some(0)).await?
    } else { Vec::new() };

    let mut resolved = Vec::new();
    for h in l2.into_iter().filter(|h| passes(h, args.filters)) { resolved.push(resolve_span_hit(h)?); }
    for h in l1.into_iter().filter(|h| passes(h, args.filters)) { resolved.push(resolve_summary_hit(h, args.root_override)?); }
    for h in l3.into_iter().filter(|h| passes(h, args.filters)) { resolved.push(resolve_week_hit(h, args.root_override)?); }
    for h in l4.into_iter().filter(|h| passes(h, args.filters)) { resolved.push(resolve_month_hit(h, args.root_override)?); }
    for h in l0.into_iter().filter(|h| passes(h, args.filters)) { resolved.push(resolve_raw_hit(h)); }

    resolved.sort_by(|a, b| b.info.score.partial_cmp(&a.info.score).unwrap_or(std::cmp::Ordering::Equal));
    let deduped = mmr_dedupe_cosine(resolved, args.mmr_threshold);

    let budget = (args.max_context_tokens * 9 / 10).max(400);
    Ok(cap_by_budget(deduped, budget))
}
```

### 5.1 New resolvers

```rust
fn resolve_week_hit(h: SearchHit, _root: Option<&str>) -> Result<ResolvedHit> {
    let window_label = h.conv_id.strip_prefix("week:").unwrap_or(&h.conv_id).to_string();
    let monday = iso_week_monday(&window_label)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(h.ts, 0).map(|d| d.date_naive())
            .unwrap_or_else(|| chrono::Utc::now().date_naive()));
    Ok(ResolvedHit {
        layer: 3,
        info: HitInfo { layer: 3, source: "week".into(), conv_id: window_label,
                        date: monday, score: similarity_of(&h) },
        snippet: h.content.clone(),
        line_hint: None,
        span_index_in_summary: None,
        vector: h.vector,
    })
}

fn resolve_month_hit(h: SearchHit, _root: Option<&str>) -> Result<ResolvedHit> {
    // parallel to resolve_week_hit; strip "month:" prefix, derive 1st-of-month via month_first_day()
    ...
}
```

### 5.2 Citation anchor format

```rust
pub fn cite_anchor(h: &ResolvedHit) -> String {
    match h.layer {
        4 => format!("[cit: {} month/{}]", h.info.date, h.info.conv_id),
        3 => format!("[cit: {} week/{}]",  h.info.date, h.info.conv_id),
        _ => { /* existing Phase 3.1 match on (line_hint, span_index_in_summary) */ }
    }
}
```

Examples:
- `[cit: 2026-04-15 cc/abc @summary-span-7]` — layer=2 (Phase 3.1, unchanged).
- `[cit: 2026-04-13 week/2026-W16]` — layer=3 (new). Date = Monday.
- `[cit: 2026-04-01 month/2026-04]` — layer=4 (new). Date = 1st of month.

### 5.3 Grounding filter

`cite::GroundingFilter` is shape-agnostic — validates by exact-match against the `valid_citations` set assembled by `prompt::render`. The new anchor shapes land in that set via `cite_anchor`; zero code change in the filter itself.

## 6. Migration, CLI, config

### 6.1 CLI: `mur conversations rollup`

```
Rollup {
    #[arg(long)] week: Option<String>,                   // "2026-W16"
    #[arg(long, conflicts_with = "week")] month: Option<String>,  // "2026-04"
    #[arg(long, conflicts_with_all = ["week", "month"])] all_missing: bool,
    #[arg(long)] force: bool,
    #[arg(long)] if_stale: bool,
    #[arg(long)] max_weeks: Option<u32>,
    #[arg(long)] max_months: Option<u32>,
}
```

### 6.2 `mur conversations compact` cascade

After `compact_missing` returns, `cmd_conversations_compact` (unless `--skip-rollups` is passed) invokes `summarize::rollup::rollup_missing(&cfg.rollup, RollupKinds::All, ...)`. Per-report output:

```
$ mur conversations compact
  2026-04-19  Written { archived: false } (18 spans, 1420ms)
  2026-04-20  Written { archived: false } (22 spans, 1810ms)
done: 2 ok, 0 failed, 0 skipped

rollup sweep:
  week 2026-W16  Written (14 spans, 2300ms)
  week 2026-W17  Noop
  month 2026-04  Written (19 spans, 3100ms)
done: 2 week ok, 1 month ok, 1 skipped
```

### 6.3 `mur conversations reindex --rollups-only`

```
Reindex {
    #[arg(long, conflicts_with_all = ["spans_only", "rollups_only"])] raw_only: bool,
    #[arg(long, conflicts_with_all = ["raw_only", "rollups_only"])] spans_only: bool,
    #[arg(long, conflicts_with_all = ["raw_only", "spans_only"])] rollups_only: bool,
}
```

Default (no flags): rebuild all — layer=0 from raw, layer=2 from day summaries, layer=3 from weekly summaries, layer=4 from monthly summaries.

`--rollups-only` walks `summary/weekly/*.md` + `summary/monthly/*.md`, parses each via `parse_summary`, embeds the narrative, upserts via `upsert_rollup_row`. Idempotent. Does NOT generate missing rollups (that's `mur conversations rollup --all-missing`'s job).

### 6.4 `mur conversations doctor`

New lines after the Phase 3.1 `spans:` line:

```
  ✓ weekly rollups: 42 rows at layer=3 (last: 2026-W16)
  ✓ monthly rollups: 11 rows at layer=4 (last: 2026-04)
```

Absent / partial:

```
  · weekly rollups: 0 indexed — run 'mur conversations rollup --all-missing'
  · monthly rollups: no weeks yet
```

Implementation: `idx.count_rows_at_layer(3)` + `count_rows_at_layer(4)` for the counts; scan `summary/weekly/` + `summary/monthly/` dirs for the "last: X" hint (most-recent filename stem).

### 6.5 Config

```rust
pub struct RollupConfig {
    #[serde(default = "rollup_default_enabled")] pub enabled: bool,                          // true
    #[serde(default = "rollup_default_max_weeks")] pub max_weeks_per_run: u32,                // 4
    #[serde(default = "rollup_default_max_months")] pub max_months_per_run: u32,              // 2
    #[serde(default = "rollup_default_max_spans_week")] pub max_extractive_spans_per_week: u32,  // 20
    #[serde(default = "rollup_default_max_words_week")] pub max_abstractive_words_per_week: u32, // 500
    #[serde(default = "rollup_default_max_spans_month")] pub max_extractive_spans_per_month: u32,// 20
    #[serde(default = "rollup_default_max_words_month")] pub max_abstractive_words_per_month: u32,// 700
    #[serde(default = "rollup_default_week_mmr")] pub week_mmr_threshold: f64,                // 0.85
    #[serde(default = "rollup_default_month_mmr")] pub month_mmr_threshold: f64,              // 0.82
    #[serde(default = "compact_default_model")] pub extractive_model: String,                 // "qwen3:14b" (via P2A helper)
    #[serde(default = "compact_default_model")] pub abstractive_model: String,                // same
    #[serde(default = "compact_default_ollama_endpoint")] pub ollama_endpoint: String,        // "http://localhost:11434"
}

impl Default for RollupConfig { ... /* all via default helpers */ }
```

Plumbed: `ConversationsConfig { ..., #[serde(default)] pub rollup: RollupConfig }`.

### 6.6 Commander P4 config sync

Phase 2A's `sync_commander_config_toml` writes managed `[conversations]` + `[conversations.compact]` TOML blocks into `~/.mur/commander/config.toml`. Phase 3.2 extends the block with `[conversations.rollup]` — same idempotent marker-delimited write. Commander's daemon doesn't consume rollup config (daemon only fires `mur conversations compact` which reads its own config.yaml); the TOML section is informational, preserving a single source of truth across both tools.

### 6.7 Path helpers (in `conversations::paths`)

```rust
pub fn weekly_summary_root(root_override: Option<&str>) -> PathBuf;
pub fn monthly_summary_root(root_override: Option<&str>) -> PathBuf;
pub fn weekly_summary_path_for(iso_week: &str, root_override: Option<&str>) -> PathBuf;
pub fn monthly_summary_path_for(yyyy_mm: &str, root_override: Option<&str>) -> PathBuf;
pub fn weekly_history_dir(root_override: Option<&str>) -> PathBuf;
pub fn monthly_history_dir(root_override: Option<&str>) -> PathBuf;
```

All derive from `summary_root(root_override).join("weekly" | "monthly")`.

### 6.8 Audit schema

New `AuditAction` variant:

```rust
Rollup {
    kind: String,                   // "week" or "month"
    window: String,                 // "2026-W16" or "2026-04"
    model: String,
    duration_ms: u64,
},
```

Hash-chain format untouched. Serde tag remains `kind` — but this now collides with the rollup's own "kind" field within the variant. Use `#[serde(rename = "action_kind")]` on the variant's `kind` field OR restructure as `Rollup { rollup_kind, window, ... }` to avoid serde-tag collision.

### 6.9 Retention for rollup `.history/`

Re-uses Phase 2C's `prune_history` pattern. Each rollup dir (`summary/weekly/.history/`, `summary/monthly/.history/`) is pruned to `history_retain` entries (default 5) on overwrite, exactly as day summaries. No new config knob.

### 6.10 Upgrade paths

1. **Fresh install** or **Phase 3.1 user with <7 days of summaries**: no rollups generated until the next ISO week closes. First daemon tick after that week's Sunday produces the weekly rollup.
2. **Phase 3.1 user with months of day summaries**: run `mur conversations rollup --all-missing`. Backfills capped by `max_weeks_per_run` / `max_months_per_run` per invocation. Next daily tick continues the catch-up. `mur conversations doctor` reports coverage.
3. **User opts out**: `conversations.rollup.enabled = false` in config.yaml. `compact` cascade short-circuits. Existing rollup files stay on disk. Retrieval still surfaces them via layer=3/4 k-NN (rows stay in LanceDB until `mur conversations reindex` is run after deleting the files).

## 7. Testing

### 7.1 Unit tests

**`summarize/rollup.rs`:**
- `iso_week_bounds` — "2026-W16" → (Mon 2026-04-13, Sun 2026-04-19). Year-boundary cases (`2026-W01`, `2025-W53`).
- `iso_week_monday` / `month_first_day` — correct derivations; unknown format → None.
- `compute_week_input_sha(day_shas)` — order-sensitive, same-input → same-sha.
- `select_spans_for_window` — 0 / 1 / duplicates / cap-exceeded / chronological ordering preserved; mixed vector/None-vector falls back to word-Jaccard via `similar()`.
- `rollup_week_writes_both_md_and_layer_3_row` — mock-mode end-to-end.
- `rollup_month_writes_both_md_and_layer_4_row` — same.
- `rollup_week_idempotent_on_same_content` — call twice, second returns Noop.
- `rollup_week_archives_prior_on_overwrite` — call with different content, prior moved to `.history/`.
- `rollup_missing_fills_multiple_weeks_under_throttle` — seed 21 days spanning 3 weeks, throttle=1 → 1 week written; next invocation → 1 more.

**`summarize/writer.rs::write_rollup`:** same idempotency + archive assertions as above, at the writer level (bypass orchestrator).

**`conversations/index.rs::scan_rows_at_layer`:**
- Empty archive → empty.
- Mixed-layer archive → only requested layer returned.
- ts range filter excludes rows outside the window.
- Vectors populated (not None).

**`conversations/index.rs::upsert_rollup_row`:**
- Writes a single row at the specified layer with the exact id/conv supplied.
- Round-trip: `search(q, k, None, Some(3))` returns the row after upsert.

**`ask/retrieve.rs` collapsed tree:**
- `collapsed_tree_surfaces_highest_scoring_layer` — seed distinct content at layer=1/2/3/4; hash-mock query matches one layer best; that layer's hit tops the output.
- `collapsed_tree_dedupes_cross_layer_duplicates` — seed a layer=2 span and a layer=3 week whose content shares most tokens; MMR drops the lower-scoring one.
- `collapsed_tree_escalates_to_layer_0_when_all_upper_empty` — only layer=0 rows in index; gather_hits returns those via escalation.
- `collapsed_tree_no_escalate_flag_blocks_layer_0` — `no_escalate=true` with only-layer=0 index → empty result.

**`ask/prompt.rs::cite_anchor`:**
- Layer=3 → `[cit: 2026-04-13 week/2026-W16]`.
- Layer=4 → `[cit: 2026-04-01 month/2026-04]`.
- Layer=2 unchanged (Phase 3.1 regression test remains green).

**`mur-common/config`:**
- `RollupConfig::default()` → all documented defaults.
- Parse a YAML with `conversations.rollup.max_weeks_per_run: 7` → override works, other fields default.

**`ollama.rs::mock_generate`:**
- Prompt containing `"Week"` → rollup-week mock narrative.
- Prompt containing `"Month"` → rollup-month mock narrative.
- Prompt containing only `"narrative paragraph"` (day compact) → existing Phase 2A behavior.

### 7.2 CLI integration tests (`tests/cli_conversations.rs`)

- `mur_conversations_rollup_week_explicit_produces_md` — seed 7 day summaries, run `--week 2026-W16`, assert file exists.
- `mur_conversations_rollup_all_missing_cascades_week_and_month` — seed ~15 days, run `--all-missing`, assert weekly files + at least one monthly if the month boundary is crossed.
- `mur_conversations_compact_cascades_into_rollup_by_default` — seed 10 days of raw, run `mur conversations compact`, verify both daily summaries and weekly rollups appear. `--skip-rollups` suppresses the cascade.
- `mur_conversations_doctor_reports_rollup_coverage` — extends the existing doctor test; asserts `"weekly rollups:"` and `"monthly rollups:"` substrings.
- `mur_conversations_reindex_rollups_only` — pre-populate disk-only weekly/monthly `.md` without layer=3/4 LanceDB rows, run `reindex --rollups-only`, verify `count_rows_at_layer(3|4)` > 0.
- `mur_ask_surfaces_month_hit_for_month_query` — seed a month's worth of days under hash-mock, run `mur ask "summarize 2026-04" --json`, assert `.hits_used[0].layer == 4`.

### 7.3 Golden-path extensions

Extend `scripts/golden-path-conversations.sh` (currently ends with "ALL 11 STEPS GREEN" after Phase 3.1):

- **Step 11.5** — seed 7 consecutive day JSONLs (one per day within a single ISO week), run `mur conversations compact` with no `--skip-rollups`; assert that `summary/weekly/*.md` exists.
- **Step 12** — `MUR_OLLAMA_MOCK=hash mur conversations rollup --all-missing --max-weeks 4 --max-months 2`; assert stdout mentions `rolled up` substring.
- **Step 13** — `mur conversations reindex --rollups-only`; `mur conversations doctor` must show `weekly rollups: >= 1`.
- **Step 14** — `MUR_OLLAMA_MOCK=hash mur ask "summarize week 2026-W16" --json`; assert `.hits_used[0].layer` is 3 OR 4 (any rollup layer confirms collapsed-tree surfaced a rollup hit).

Banner updated: `=== ALL 15 STEPS GREEN ===`.

### 7.4 Mock coverage

No new mock-mode flag. `MUR_OLLAMA_MOCK=hash` (Phase 3.1) + the existing `mock_generate` prompt-substring routing (with the two new `"Week"` / `"Month"` branches in 7.1) handle deterministic rollup behavior.

### 7.5 Real-Ollama smoke

Phase 2C's `ollama-live-smoke` feature-gated test seeds 1 day and runs `compact`. After Phase 3.2, extend the seed to 7 consecutive days (one ISO week) so the cascade also triggers `rollup_week`. One new assertion: `summary/weekly/*.md` was produced after the CLI returns. No schema change; no new feature flag.

### 7.6 Test isolation

All new tokio tests that mutate `MUR_OLLAMA_MOCK` use the shared `conversations::ENV_LOCK` (Phase 2 pattern). All CLI integration tests use `MUR_HOME` override (Phase 2C pattern).

## 8. Success criteria

Phase 3.2 ships when, on a seeded archive:

1. After 7 seeded consecutive days + one `mur conversations compact` invocation (with default cascade), exactly one weekly `.md` exists + LanceDB has ≥1 layer=3 row. Verified via hash-mock integration test.
2. After 35 seeded days spanning a month boundary + one `rollup --all-missing` invocation, weekly and monthly `.md` files exist on disk + corresponding layer=3 and layer=4 rows in LanceDB.
3. `mur ask "summarize 2026-04" --json` with a populated archive returns a citation whose `layer == 4` (month) under hash-mock, and the JSON `answer` references the month narrative.
4. Existing Phase 3.1 tests all pass (backward compat).
5. `cargo test --workspace` green under default parallelism; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --check` clean.
6. Golden path's new Steps 11.5 / 12 / 13 / 14 all pass; banner updated.

## 9. Risks & open questions

**Risks:**

- **Rollup generation latency on backfill.** A fresh `mur conversations rollup --all-missing` on a user with 180 days of summaries runs ~26 weekly + 6 monthly rollups = 32 LLM calls to Ollama. At ~5 s each on a local qwen3:14b, that's ~3 minutes. Acceptable for a one-time upgrade action; throttle defaults (4 weeks, 2 months per run) keep the daily cascade cost bounded if the daemon is backfilling.
- **LanceDB row-count at scale.** Per year: +52 weekly + 12 monthly ≈ 64 new rows vs +20×365 = 7300 span rows. Negligible.
- **MMR threshold interaction between layers.** Cross-layer dedupe via cosine may drop month-layer hits when they overlap with day-layer hits, even though the week/month hits could be more informative for broad queries. Mitigation: the global sort-by-score before MMR means the higher-scoring hit wins; for broad queries the month-layer hit scores highest naturally. If this bites in practice, Phase 3.2.1 can add per-layer-floor (keep at least 1 hit per layer in the final pool).
- **Pseudo-source `"week"` / `"month"` column values** don't round-trip through `Source::from_prefix`. This doesn't break retrieval (resolvers use `h.conv_id` prefix, not `h.source`), but it does mean `ConversationIndex::search`'s existing source-string match needs a fallback for unknown prefixes (map to `Source::ClaudeCode` as a placeholder, since callers filter by layer not source for rollup rows).
- **Audit variant tag collision.** `AuditAction` is serde-tagged `kind`, and our new variant has its own `kind` field. Must rename the variant's field to avoid serialization conflict.

**Open questions:**

- **Should layer=3/4 rows be filterable by the underlying day sources?** Currently `filters.source` filters on the row's `source` column, which for rollups is synthetic `"week"`/`"month"`. If a user passes `--src cc` to `mur ask`, they'd expect to exclude rollup rows too (rollups aggregate across sources). Punt to future work: for Phase 3.2, source filtering only applies to layers 0/1/2. Document in the CLI help.
- **MMR threshold values for week/month rollups** (0.85 and 0.82 respectively vs ask's 0.88) — chosen by gut, not empirically. If results are too sparse or too redundant in practice, tune via config. Non-blocking.

## 10. References

- Phase 3.1 spec: `docs/superpowers/specs/2026-04-21-mur-conversations-phase-3-1-design.md`
- Phase 3.1 merge: `ed7f883`
- Phase 2C merge (reindex baseline + doctor coverage): `a31690d`
- RAPTOR paper (Sarthi et al., 2024) — https://arxiv.org/abs/2401.18059 — hybrid pipeline is inspired by the paper's tree-construction idea, simplified for a single-user archive (no clustering step).
- Phase 2A spec §10.4 (deferred Phase 3 list, weekly/monthly summaries at item 4).

# mur Conversations Phase 3.2.1 — `--src` Rollup Fix + `--if-stale` No-op Design

**Status:** Approved 2026-04-22
**Depends on:** Phase 3.4 shipped (`958b48c`).
**Type:** Focused follow-up fixing two known Phase 3.2 gaps documented inline in the code + in the Phase 3.2 shipped memory file.

---

## 1. Purpose

Two independent bugs from Phase 3.2 surfaced as documented gaps during the Phase 3.3 and 3.4 reviews but were deferred:

1. **`--src` filter silently drops layer=3/4 rollup hits.** `mur ask --src cc` passes `Some(Source::ClaudeCode)` as a LanceDB predicate `source = 'cc'` into every k-NN search. Rollup rows store synthetic strings (`source = "week"` / `"month"`), so they never match and are excluded at the DB level. User asks a broad-scope question like `"what did I ship in March?" --src cc` and gets zero rollup hits with no warning.

2. **`--if-stale` on `mur conversations rollup` is a silent alias for `--force`.** The `cmd_conversations_rollup` handler does `let force = args.force || args.if_stale;`. But `--if-stale` semantically means "regenerate only if stale" — which is already the default behavior (`force=false` triggers the internal `input_content_sha` check). The flag is misleading and wrong.

Both are small, mechanical, and unblocked. Bundling them as Phase 3.2.1.

## 2. Non-goals

- **Storing per-rollup source contributor mask** (Q1 option b). Semantically cleanest but over-engineered: needs schema evolution, migration, reindex path, and handles ambiguous edge cases poorly (rollup built from 1% cc content — include or exclude?). Deferred permanently unless measurement shows users actually want filtered-rollup behavior.
- **Removing `--if-stale` flag entirely.** Kept as a no-op for backward compatibility; existing scripts that pass it continue to work.
- **New config surface.** No new `AskConfig` or `RollupConfig` fields.
- **Diagnostic output surfacing dropped rollups.** Not needed under chosen semantic (a) — rollups now always surface regardless of filter.
- **Changing `mur conversations compact --if-stale` semantic.** That one IS correctly implemented (passes `args.if_stale` to `compact_missing` separately from `force`). This fix is rollup-subcommand only.

## 3. Design decisions

### 3.1 `--src` rollup inclusion: option (a) always include

Passing `--src cc` no longer filters layer=3/4 rollup hits. Rationale: rollups are multi-source aggregates by construction; they synthesize across the user's enabled sources and cannot faithfully be scoped to one source without storing a contributor-mask (Q1 option b) that we explicitly deferred.

Under option (a), `--src cc` answers "only show me cc day-level content" while rollup hits surface based on embedding relevance. Users who don't want rollups in filtered answers can still exclude them by avoiding broad-scope queries that retrieve at the week/month layer; rollups rarely surface when the query is narrow.

Trade-off accepted: a filtered `--src cc` query may include rollup content that references non-cc work. The help text documents this explicitly.

### 3.2 `--if-stale` becomes a no-op

The clap flag remains (backward compatibility). Help text clarifies it's now documented as no-op backed by default behavior. `cmd_conversations_rollup` stops OR'ing it into `force`; `force` equals `args.force` only. The rollup orchestrator's existing `input_content_sha` check naturally skips regeneration when content is fresh.

## 4. Architecture

Two files mutated per fix:

| File | Change | Fix |
|---|---|---|
| `mur-core/src/conversations/ask/retrieve.rs` | Layer=3 and layer=4 k-NN calls in `gather_hits` pass `None` instead of `primary_src`; update inline doc comment | 1 |
| `mur-core/src/main.rs` | `--src` help text wording updated | 1 |
| `mur-core/src/cmd/conversations_cmd.rs` | Two `let force = args.force || args.if_stale;` → `let force = args.force;` in `cmd_conversations_rollup` | 2 |
| `mur-core/src/main.rs` | `--if-stale` help text on `ConversationsAction::Rollup::if_stale` wording updated | 2 |
| `mur-core/src/conversations/ask/retrieve.rs` | 1 new unit test | 1 |
| `mur-core/tests/cli_conversations.rs` | 2 new integration tests | 2 |

No new Cargo dependencies. No LanceDB schema changes. No migration. No commander sync changes. No golden-path changes (Phase 3.3's 17 steps stay green on default queries where neither fix affects output).

## 5. Fix 1 — `--src` + rollups

### 5.1 Current behavior

`gather_hits` in `mur-core/src/conversations/ask/retrieve.rs` (around lines 47-61):

```rust
let primary_src = args.filters.source.first().copied();
let l2 = idx.search(&args.query_embedding, k_each, primary_src, Some(2)).await?;
let l1 = idx.search(&args.query_embedding, k_each, primary_src, Some(1)).await?;
let l3 = idx.search(&args.query_embedding, k_each, primary_src, Some(3)).await?;
let l4 = idx.search(&args.query_embedding, k_each, primary_src, Some(4)).await?;
```

When `primary_src = Some(Source::ClaudeCode)`, the LanceDB predicate becomes `source = 'cc'`. Rollup rows have `source = 'week'` or `'month'` — never match.

### 5.2 New behavior

Layer=3 and layer=4 k-NN calls pass `None` instead, so rollups are included based purely on embedding relevance:

```rust
let primary_src = args.filters.source.first().copied();
let l2 = idx.search(&args.query_embedding, k_each, primary_src, Some(2)).await?;
let l1 = idx.search(&args.query_embedding, k_each, primary_src, Some(1)).await?;
// Phase 3.2.1: rollup rows are multi-source aggregates; source filter
// doesn't apply to them. Pass None so they surface based purely on
// relevance even under --src.
let l3 = idx.search(&args.query_embedding, k_each, None, Some(3)).await?;
let l4 = idx.search(&args.query_embedding, k_each, None, Some(4)).await?;
```

The inline Phase 3.2 doc comment on `primary_src` (currently lines 42-46, describing the original breakage) is replaced with a Phase 3.2.1 comment describing the new inclusion semantic.

### 5.3 `--src` CLI help text update

In `mur-core/src/main.rs`, the `Commands::Ask::src` field (around line 351):

```rust
/// Filter results to a specific source (e.g. "cc", "cursor").
/// Phase 3.2 note: source filtering does NOT apply to weekly/monthly
/// rollup hits (layer=3/4) — rollup rows use synthetic source strings
/// ("week"/"month") and are always excluded when --src is passed.
#[arg(long)]
src: Option<String>,
```

Update the comment to match the new semantic:

```rust
/// Filter results to a specific source (e.g. "cc", "cursor").
/// Phase 3.2.1: source filter applies ONLY to day-level content
/// (layers 0/1/2). Weekly/monthly rollup hits (layers 3/4) are
/// multi-source aggregates and always surface based on relevance.
#[arg(long)]
src: Option<String>,
```

### 5.4 Test

New unit test in `mur-core/src/conversations/ask/retrieve.rs` (adjacent to the existing `gather_hits_*` tests):

```rust
#[tokio::test]
async fn gather_hits_rollup_surfaces_despite_src_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();

    // Seed layer=2 cc span + layer=3 rollup (synthetic "week" source).
    let s = make_msg("c_span", "span text");
    idx.upsert_with_layer(&[(s, vec![0.7; 16], 2)]).await.unwrap();
    idx.upsert_rollup_row(crate::conversations::index::RollupRow {
        id: "wk_2026-W16_L3_0",
        ts: 0,
        source: "week",
        conv_id: "week:2026-W16",
        layer: 3,
        content: "week narrative",
        vector: &vec![0.7; 16],
    }).await.unwrap();

    // Query with --src cc filter active.
    let args = RetrieveArgs {
        query_embedding: vec![0.7; 16],
        filters: &Filters {
            source: vec![Source::ClaudeCode],
            since: None,
            until: None,
            min_score: 0.0,
        },
        k_summary: 8,
        k_raw: 4,
        escalation_threshold: 0.3,
        mmr_threshold: 0.95,
        no_escalate: false,
        max_context_tokens: 6000,
        root_override: Some(root),
    };
    let hits = gather_hits(args).await.unwrap();
    let layers: Vec<i8> = hits.iter().map(|h| h.layer).collect();
    assert!(layers.contains(&2), "cc layer=2 must survive; layers: {layers:?}");
    assert!(layers.contains(&3), "layer=3 rollup must surface despite --src filter; layers: {layers:?}");
}
```

## 6. Fix 2 — `--if-stale` no-op

### 6.1 Current behavior

`cmd_conversations_rollup` in `mur-core/src/cmd/conversations_cmd.rs` (lines ~868-877):

```rust
if let Some(w) = args.week {
    let force = args.force || args.if_stale;
    let r = rollup_week(&w, force, &rollup_cfg, None).await?;
    ...
}
if let Some(m) = args.month {
    let force = args.force || args.if_stale;
    let r = rollup_month(&m, force, &rollup_cfg, None).await?;
    ...
}
```

The OR means `--if-stale` sets `force=true`, bypassing the idempotency check — wrong semantics.

### 6.2 New behavior

```rust
if let Some(w) = args.week {
    // Phase 3.2.1: --if-stale is a no-op; the default (force=false)
    // already triggers the sha-based idempotency check inside
    // rollup_week. --if-stale retained for backward-compat with scripts.
    let force = args.force;
    let r = rollup_week(&w, force, &rollup_cfg, None).await?;
    ...
}
if let Some(m) = args.month {
    let force = args.force;
    let r = rollup_month(&m, force, &rollup_cfg, None).await?;
    ...
}
```

(The `rollup_missing` sweep path at the bottom of the handler does not use `args.force` or `args.if_stale` — it always calls `rollup_missing` which internally uses `force=false`. Unchanged.)

### 6.3 CLI help text update

In `mur-core/src/main.rs`, the `ConversationsAction::Rollup::if_stale` field:

```rust
/// Only regenerate when source content hash changed.
#[arg(long)]
if_stale: bool,
```

Update to:

```rust
/// Only regenerate when source content hash changed (Phase 3.2.1:
/// this is the default; flag is a no-op kept for backward compat —
/// omit --force to get staleness-checking behavior).
#[arg(long)]
if_stale: bool,
```

### 6.4 Tests

Two integration tests in `mur-core/tests/cli_conversations.rs`:

**`mur_conversations_rollup_if_stale_is_idempotent_noop`:**

Seeds a week rollup via the normal flow, runs `mur conversations rollup --week 2026-W16 --if-stale`, asserts second call does NOT create a new `.history/` archive entry (proof that `--if-stale` does not force regeneration).

**`mur_conversations_rollup_force_still_regenerates`:**

Same setup, runs `mur conversations rollup --week 2026-W16 --force` twice, asserts second call creates a `.history/` archive entry (proof that `--force` still bypasses the sha check).

Both tests confirm the fix preserves `--force` behavior while correcting `--if-stale`.

## 7. Success criteria

All of the following true at merge:

- `cargo test --workspace` green (existing + 3 new tests).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo fmt --check` clean.
- `./scripts/golden-path-conversations.sh` still prints `=== ALL 17 STEPS GREEN ===` — no regression.
- Manual verification: `mur ask "summarize recent work" --src cc --json` returns at least one layer=3 or layer=4 hit (if any exist in the archive) — surfaces rollups under filter.
- Manual verification: `mur conversations rollup --week 2026-W16 --if-stale` run twice produces a single JSONL session entry; archive dir is empty or unchanged between runs.

## 8. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Users relying on silent rollup exclusion under `--src` see new cross-source content in filtered answers | Help text documents the new behavior clearly. Existing inline comment in code also updated. |
| Breaking scripts that use `--if-stale` as an alias for `--force` | Flag retained (not removed) — no breaking change. Behavior is stricter now (default staleness-check), which is what users pass `--if-stale` to get anyway. |
| Edge case: rollup rows coexist with layer=2 cc spans and the mmr dedupe collapses the cc span in favor of a rollup | Existing mmr_threshold behavior handles this; unchanged by fix. |

## 9. References

- Phase 3.2 design spec: `docs/superpowers/specs/2026-04-21-mur-conversations-phase-3-2-design.md` (§9 lists both gaps as open questions deferred to follow-up).
- Phase 3.2 shipped memory: `project_conversations_phase_3_2_shipped.md` — "`--src` rollup gap deferred to 3.3" and "`--if-stale` semantic fix on rollup subcommand" known deferrals.
- Phase 3.2 inline comment: `mur-core/src/conversations/ask/retrieve.rs` line 42-46 (to be updated).
- Phase 3.2 inline comment: `mur-core/src/cmd/conversations_cmd.rs` around line 870 (to be updated).

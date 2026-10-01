# LanceDB 0.39 feature adoption (option B) — design notes

**Status:** parked. Resume after the option A upgrade
(`2026-10-01-lancedb-0.39-upgrade-design.md`) is merged.
**Scope:** which LanceDB 0.39 capabilities are worth adopting in MUR, split into
independent sub-projects. Each sub-project gets its own design pass when picked
up; this document only records the inventory, the evidence, and the order.

## 1. Feature inventory — verified against the Rust crate

Claims circulating about "LanceDB 0.26 → 0.39" mix Python-only, cloud-only, and
Rust-crate features. Each row below was checked by grepping
`lancedb-0.39.0/src` in the local cargo registry, not taken from release notes.

| Claim | In Rust 0.39 source | Value to MUR |
|---|---|---|
| Native FTS (BM25) + hybrid query + reranking | Yes — `Index::FTS`, `query.rs:50 mod hybrid`, `rerankers/rrf.rs` (`RRFReranker`) | **High** — can replace the separate tantivy index |
| Cross-field FTS (BM25F / `combined_fields`) | Partial — `FtsQuery::MultiMatch` exists; no `BM25F` / `combined_fields` identifiers found | Medium — search title + content together |
| NGram / BloomFilter / RTree scalar indexes | Not found | — |
| Substring index | Yes — `Index::Fm` (`index.rs:55`), accelerates `contains(col, '...')` | **High** — exact identifier matching for code search |
| Versions, tags, restore | Yes — `tags`, `checkout_tag`, `restore` | Medium — rollback for a failed reindex |
| Branches | Yes — `table.rs:2346 create_branch` | Low for a local-first store |
| Materialized views, namespaces | Modules present | Low — lakehouse / multi-tenant cloud oriented |
| RaBitQ (`IvfRq`), `IvfHnswSq` | Yes | Not now — see §2.4 |
| Python UDFs, haswell wheels, `lancedb-compat` | Python SDK only | None |
| `lzma-sys` static linking advice | `lzma-sys` absent from the current `Cargo.lock` | Re-check with `cargo tree -i lzma-sys` after the bump |

## 2. Current pain points (evidence)

### 2.1 Two indexes, merged by hand

`mur-core/src/retrieve/unified.rs:47`:

```rust
    // Sources: combine vector hits + BM25 hits.
```

BM25 comes from a separate tantivy index (`mur-core/src/sources/tantivy.rs`)
stored apart from the LanceDB tables, so the two can drift.

### 2.2 Codebase search is vector-only

`mur-core/src/codebase/mod.rs` queries with `nearest_to` only. Identifier
lookups (e.g. `canonicalize_agent_name`) are a known weak spot for pure
semantic search.

### 2.3 Upsert is not atomic and swallows errors

`mur-core/src/store/vector/lancedb.rs:366`:

```rust
        let _ = table.delete(&predicate).await;
```

Delete-then-add: a crash between the two loses rows, and a failed delete is
silently ignored. `merge_insert` makes this a single commit.

### 2.4 ANN index is a stub — and not needed yet

`mur-core/src/conversations/index.rs:424-426`:

```rust
        // LanceDB 0.26: `create_index` with IvfPq / Hnsw. RaBitQ is available
        // via IndexType::RaBitQ when the feature is present; fall back to IVF_PQ.
        // TODO(phase-2): pin the exact call once LanceDB API stabilizes.
```

Measured on one dev install: `patterns.lance` 28K, `sources.lance` 184K,
codebase index 1.5M. Flat scan is adequate at this size; an ANN index (RaBitQ
or otherwise) buys nothing until tables are orders of magnitude larger.
Any future trigger must be a row-count threshold from config, not a hardcoded
number.

## 3. Sub-projects

All four depend on option A being merged. They are independent of each other.

| # | Sub-project | Summary | Notes |
|---|---|---|---|
| 1 | **Native hybrid search** | LanceDB FTS + `Fm` substring index + RRF reranking; retire `sources/tantivy.rs`; add keyword/identifier search to codebase | Recommended first: most user-visible, removes a duplicate index |
| 2 | **Atomic upsert** | Replace delete+add with `merge_insert`; surface delete/merge errors | Small; candidate to fold into option A instead of a separate design |
| 3 | **Index version guard** | Tag before reindex, `restore` on failure; optionally tag the codebase index with the git commit it was built from | |
| 4 | **Maintenance schedule** | Daemon runs `optimize()` periodically to compact fragments; ANN deferred per §2.4 | Interval from config |

## 4. Open questions (to resolve when resumed)

1. Start with sub-project 1, or fold 2 into option A first and then design 1?
2. Sub-project 1: migration path for existing tantivy indexes — rebuild from
   source rows on first run, or keep tantivy as a fallback for one release?
3. Sub-project 1: is `MultiMatch` sufficient for title + content scoring, or is
   a per-field score fusion needed?
4. Sub-project 3: how many tags to retain, and who prunes them (ties into 4)?
5. Re-verify every row of §1 against the exact lancedb version actually locked
   after option A lands — the inventory above was taken against 0.39.0.

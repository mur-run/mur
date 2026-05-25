# M6c.1 — LanceDB Skill Vector Dedup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Augment `mur skill consolidate` with cosine-similarity dedup driven by LanceDB skill embeddings. M5b's Jaccard pass is kept as-is; vector dedup is opt-in via a `--method` flag. No schema migration, no LLM, no MCP dependency — pure incremental win over M5b's token-set heuristic.

**Spec mapping:** §9.4 consolidation (dedup), §M6 LanceDB-replacement-for-Jaccard bullet (M5a §"Out of scope" #5, M5b §"Out of scope" #1).

**Hard dependency on M5b:**
- `mur-core/src/skill_consolidate/{mod.rs,dedup.rs,report.rs}` exist and ship the Jaccard pass + `ConsolidateReport` JSONL writer.
- `mur skill consolidate [--dry-run] [--apply]` CLI surface exists.
- `SkillView` (the per-skill snapshot the passes consume) and `ConsolidateReport.duplicates: Vec<DuplicatePair>` are stable.

Required when M5b lands. M6c.1 PR rebases on M5b's branch.

**What M6c.1 ships:**
1. `mur skill consolidate --method=vector|jaccard|both` flag (default `jaccard` to preserve M5b behaviour).
2. `mur-core/src/skill_consolidate/dedup_vec.rs` — cosine-similarity dedup pass.
3. Skill embedding index in LanceDB under `source_id = "skill"`, populated on install + via `mur skill reindex-vec`.
4. `mur skill reindex-vec` — rebuild the skill embedding index for all installed skills (mirrors the pattern `mur reindex` command).
5. Telemetry: `Event::SkillIndexed { skill_name, skill_version, dims }`.
6. Documentation that `--method=both` reports the union of findings and tags each `DuplicatePair` with `source: "jaccard" | "vector" | "both"`.

**What M6c.1 does NOT ship:**
- LanceDB index for pattern → skill cross-references (just skill ↔ skill).
- LLM-driven adjudication of low-confidence dedup matches → M6c.
- Replacing Jaccard. Jaccard stays the default until enough field data justifies a default-flip; that decision is explicitly out of scope here.
- Auto-embed on update of an installed skill — only `install` and `reindex-vec` populate the index. Stats / lifecycle changes do not re-embed.
- Approximate-nearest-neighbour index tuning (`CREATE INDEX`). M6c.1 uses LanceDB's default ANN config; tuning is its own future task if recall is insufficient.

**Tech Stack:** Rust 2024. Reuse existing `LanceDbStore` (`mur-core/src/store/vector/lancedb.rs`) and `EmbeddedChunk` schema with `source_id = "skill"`. Reuse the existing embedder (`mur-core/src/store/embedding.rs`). **No new dependencies.**

**Deployment assumption:** Single-host. LanceDB skill index lives at `~/.mur/lance/` (same dir as pattern index). NFS-mounted MUR_HOME is out of scope (inherited from M5a/M5b).

---

## File Structure

**Create:**
- `mur-core/src/skill_consolidate/dedup_vec.rs` — cosine-similarity pass + keeper-selection reusing M5b's tiebreaker logic.
- `mur-core/src/skill_index/mod.rs` — skill embedding index helpers (text builder, embed-and-upsert).
- `mur-core/src/skill_index/text.rs` — canonical text representation for embedding (`name + description + abstract + triggers + first procedure step intent`).
- `mur-core/src/cmd/skill_reindex_vec.rs` — `mur skill reindex-vec` CLI dispatcher.
- `mur-core/tests/skill_dedup_vec.rs` — fixture-driven test with three near-duplicate skills + one distant.
- `mur-core/tests/skill_reindex_vec.rs` — end-to-end: install three skills, reindex-vec, search, assert top-k.

**Modify:**
- `mur-core/src/skill_consolidate/mod.rs` — register the new pass; thread `Method` enum through the orchestrator.
- `mur-core/src/skill_consolidate/dedup.rs` — annotate emitted `DuplicatePair` with `source: DedupSource::Jaccard` so combined-mode output is unambiguous.
- `mur-core/src/skill_consolidate/report.rs` — add `source` field to `DuplicatePair` serialization; add a `method` field at the top of the report.
- `mur-core/src/cmd/skill_consolidate.rs` — add `--method=jaccard|vector|both` flag (default `jaccard`).
- `mur-core/src/cmd/skill_install.rs` (or wherever install lives) — on successful install, call `skill_index::embed_and_upsert`.
- `mur-agent-runtime/src/telemetry_writer.rs` — add `Event::SkillIndexed { skill_name, skill_version, dims, duration_ms }` + `event_to_notification` arm.
- `mur-core/src/lib.rs` — `pub mod skill_index;`.
- `mur-core/src/main.rs` (or `cli.rs`) — wire the `mur skill reindex-vec` subcommand.

**Do not modify:**
- `SkillManifest` / `Skill` structs in `mur-common::skill::manifest` — vector dedup operates on already-loaded skill content, no schema changes.
- DSSE signing — embeddings are runtime-derived, never signed.
- M5b's Jaccard `dedup.rs` behaviour — only the `DuplicatePair` annotation is touched.
- `SkillStats` schema — embedding-index membership is tracked by LanceDB itself, not by sidecar fields. (Schema-evolution policy from M5b Task 0 applies if this changes.)

---

### Task 1 — Skill embedding index helpers

**Files:** `mur-core/src/skill_index/{mod.rs,text.rs}` (new).

- [ ] **Step 1: Canonical text representation**

```rust
// mur-core/src/skill_index/text.rs

use mur_common::skill::manifest::Skill;

/// Build the canonical text we embed for a skill. Stable: changing this
/// invalidates every embedded chunk and requires `mur skill reindex-vec`.
///
/// Format (newline-joined, no trailing newline):
///   <name>
///   <description>
///   <abstract>
///   <trigger_keywords joined by spaces, sorted>
///   <first_procedure_step_description if any>
///
/// Order matters: name and description dominate the embedding, abstract
/// adds semantic context, triggers cover keyword-match cases.
pub fn embed_text(skill: &Skill) -> String {
    let m = &skill.manifest;
    let mut parts = vec![m.name.clone(), m.description.clone(), m.content.r#abstract.clone()];

    let mut triggers: Vec<&str> = m.triggers.iter()
        .filter_map(|t| t.exact_keyword())   // Only exact-string triggers; skip glob/regex
        .collect();
    triggers.sort();
    if !triggers.is_empty() { parts.push(triggers.join(" ")); }

    if let Some(proc) = &m.content.procedure {
        if let Some(first) = proc.steps.first() {
            parts.push(first.description.clone());
        }
    }
    parts.join("\n")
}
```

Note: `Trigger::exact_keyword` is a small helper to add on `mur-common::skill::manifest::Trigger` returning `Option<&str>` only for `TriggerKind::Keyword` entries. If that helper doesn't exist yet, add it as part of Step 1 — pure read accessor, no schema change.

- [ ] **Step 2: Embed-and-upsert helper**

```rust
// mur-core/src/skill_index/mod.rs

pub mod text;

use crate::store::embedding::Embedder;
use crate::store::vector::{EmbeddedChunk, VectorStore};
use chrono::Utc;
use mur_common::skill::manifest::Skill;

pub const SKILL_SOURCE_ID: &str = "skill";

pub async fn embed_and_upsert(
    skill: &Skill,
    embedder: &dyn Embedder,
    store: &dyn VectorStore,
) -> anyhow::Result<usize> {
    let text = text::embed_text(skill);
    let vec = embedder.embed(&text).await?;
    let dims = vec.len();

    let chunk = EmbeddedChunk {
        chunk_id: format!("skill:{}:{}", skill.manifest.name, skill.manifest.version),
        source_id: SKILL_SOURCE_ID.into(),
        external_id: skill.manifest.name.clone(),
        ordinal: 0,
        text,
        heading_path: vec![],
        char_range: (0, 0),
        updated_at: Utc::now(),
        embedding: vec,
    };
    store.upsert(&[chunk]).await?;
    Ok(dims)
}

pub async fn delete(skill_name: &str, store: &dyn VectorStore) -> anyhow::Result<()> {
    store.delete_by_external_ids(SKILL_SOURCE_ID, &[skill_name.to_string()]).await
}
```

- [ ] **Step 3: Build + commit**

```
cargo build -p mur-core
cargo test -p mur-core --test ... # whatever exists; new test added at Task 2
git add mur-core/src/skill_index/ mur-core/src/lib.rs
git commit -m "feat(skill): skill embedding index helpers"
```

---

### Task 2 — Cosine-similarity dedup pass

**Files:** `mur-core/src/skill_consolidate/dedup_vec.rs` (new), `mur-core/src/skill_consolidate/{mod.rs,dedup.rs,report.rs}` (modify).

- [ ] **Step 1: Annotate existing `DuplicatePair` with source**

```rust
// mur-core/src/skill_consolidate/report.rs (modify)

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DedupSource {
    Jaccard,
    Vector,
    Both,    // Pair surfaced by both passes; report uses this when combining.
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicatePair {
    pub keeper: String,
    pub loser: String,
    pub similarity: f64,           // Jaccard score or cosine similarity, depending on source.
    #[serde(default)]
    pub source: DedupSource,
}

impl Default for DedupSource {
    fn default() -> Self { Self::Jaccard }  // Backwards-compat: old reports without source = Jaccard.
}
```

Update M5b's `dedup.rs` to stamp `source: DedupSource::Jaccard` on every emitted pair (one-line change in the existing `report.duplicates.push`).

- [ ] **Step 2: Vector dedup pass**

```rust
// mur-core/src/skill_consolidate/dedup_vec.rs

use super::report::{ConsolidateReport, DedupSource, DuplicatePair, SkillView};
use crate::skill_index::{SKILL_SOURCE_ID, text};
use crate::store::embedding::Embedder;
use crate::store::vector::{SearchFilter, VectorStore};

pub const COSINE_THRESHOLD: f32 = 0.92;
pub const TOP_K: usize = 8;

pub async fn scan(
    skills: &[SkillView],
    embedder: &dyn Embedder,
    store: &dyn VectorStore,
    report: &mut ConsolidateReport,
) -> anyhow::Result<()> {
    // For each skill we already have its embedding in the index (populated at install / reindex-vec).
    // Strategy: embed the canonical text of skill[i], search top-K against `source_id = "skill"`,
    // skip self, emit pairs above threshold. Symmetry de-duplicated by ordered (a,b) with a < b.

    let filter = SearchFilter { source_ids: Some(vec![SKILL_SOURCE_ID.into()]), since: None };

    for s in skills {
        let q = embedder.embed(&text::embed_text(&s.skill)).await?;
        let hits = store.search(&q, TOP_K, &filter).await?;
        for hit in hits {
            if hit.external_id == s.skill.manifest.name { continue; }
            if hit.score < COSINE_THRESHOLD { continue; }

            // Order pair lexicographically to make symmetric matches deterministic.
            let (a, b) = if s.skill.manifest.name < hit.external_id {
                (s.skill.manifest.name.clone(), hit.external_id.clone())
            } else {
                (hit.external_id.clone(), s.skill.manifest.name.clone())
            };

            // Keeper selection — reuse M5b's helper (extracted into `report::pick_keeper` if not already public).
            let (keeper, loser) = super::report::pick_keeper(&a, &b, skills);
            report.duplicates.push(DuplicatePair {
                keeper, loser,
                similarity: hit.score as f64,
                source: DedupSource::Vector,
            });
        }
    }
    dedup_combined(report);
    Ok(())
}

/// Walk emitted pairs; collapse (a,b) appearing under both Jaccard and Vector
/// into a single entry with `source = Both`. Preserves the higher similarity.
fn dedup_combined(report: &mut ConsolidateReport) {
    use std::collections::HashMap;
    let mut by_pair: HashMap<(String, String), DuplicatePair> = HashMap::new();
    for p in report.duplicates.drain(..) {
        let key = if p.keeper < p.loser { (p.keeper.clone(), p.loser.clone()) } else { (p.loser.clone(), p.keeper.clone()) };
        by_pair.entry(key)
            .and_modify(|existing| {
                if existing.source != p.source { existing.source = DedupSource::Both; }
                if p.similarity > existing.similarity { existing.similarity = p.similarity; }
            })
            .or_insert(p);
    }
    report.duplicates = by_pair.into_values().collect();
    report.duplicates.sort_by(|a, b| a.keeper.cmp(&b.keeper).then(a.loser.cmp(&b.loser)));
}
```

- [ ] **Step 3: Threshold rationale + tests**

`COSINE_THRESHOLD = 0.92` is the starting value. Justification belongs in the doc comment above the constant: it should match a sentence-transformer's "very similar but not identical" band. The test in Task 4 includes a fixture verifying the threshold rejects loose conceptual overlap (e.g., two skills both about "search" but with different operational intent should NOT dedup).

- [ ] **Step 4: Build + commit**

```
cargo build -p mur-core
git add mur-core/src/skill_consolidate/{dedup_vec.rs,report.rs,dedup.rs,mod.rs}
git commit -m "feat(skill): cosine-similarity dedup pass for consolidate"
```

---

### Task 3 — Wire to `mur skill consolidate --method=<...>`

**Files:** `mur-core/src/cmd/skill_consolidate.rs` (modify), `mur-core/src/skill_consolidate/mod.rs` (modify).

- [ ] **Step 1: Add CLI flag**

```rust
#[derive(clap::ValueEnum, Clone, Debug)]
pub enum Method { Jaccard, Vector, Both }

#[derive(clap::Parser, Debug)]
pub struct ConsolidateArgs {
    // ... existing args
    #[arg(long, value_enum, default_value_t = Method::Jaccard)]
    pub method: Method,
}
```

Default `Jaccard` — explicit decision: vector dedup is opt-in until field data justifies a default flip (revisit in M7 or whenever recall data exists).

- [ ] **Step 2: Thread through orchestrator**

```rust
// mur-core/src/skill_consolidate/mod.rs

pub async fn run(args: &ConsolidateArgs, ctx: &Ctx) -> anyhow::Result<ConsolidateReport> {
    let skills = load_skill_views(ctx)?;
    let mut report = ConsolidateReport::new(args.method.clone());

    match args.method {
        Method::Jaccard => dedup::scan(&skills, &mut report)?,
        Method::Vector  => dedup_vec::scan(&skills, &*ctx.embedder, &*ctx.vector_store, &mut report).await?,
        Method::Both => {
            dedup::scan(&skills, &mut report)?;
            dedup_vec::scan(&skills, &*ctx.embedder, &*ctx.vector_store, &mut report).await?;
            // dedup_combined() already runs inside dedup_vec::scan; safe to call again is a no-op,
            // but verify by writing a test that runs Both twice and asserts idempotent report.
        }
    }

    contradiction::scan(&skills, &mut report)?;
    orphan::scan(&skills, &mut report, ctx.now)?;

    if args.apply { apply::apply(&report, ctx)?; }
    report::write_jsonl(&report, ctx)?;
    Ok(report)
}
```

- [ ] **Step 3: Surface method in the JSONL report header**

```rust
// mur-core/src/skill_consolidate/report.rs

#[derive(Debug, Clone, Serialize)]
pub struct ConsolidateReport {
    pub run_id: String,
    pub method: Method,                   // NEW — first JSONL line is the header.
    pub started_at: DateTime<Utc>,
    pub duplicates: Vec<DuplicatePair>,
    pub contradictions: Vec<ContradictionPair>,
    pub orphans: Vec<OrphanFinding>,
}
```

- [ ] **Step 4: Build + commit**

```
cargo build -p mur-core
git add mur-core/src/cmd/skill_consolidate.rs mur-core/src/skill_consolidate/mod.rs
git commit -m "feat(skill): consolidate --method=jaccard|vector|both"
```

---

### Task 4 — Auto-embed on install + `mur skill reindex-vec`

**Files:** `mur-core/src/cmd/skill_install.rs` (modify), `mur-core/src/cmd/skill_reindex_vec.rs` (new), `mur-core/src/main.rs` or `cli.rs` (modify).

- [ ] **Step 1: Embed on install**

After successful `install` (i.e., the manifest is on disk, signature verified, `mur skill list` would now show it), call:

```rust
let dims = skill_index::embed_and_upsert(&skill, &*embedder, &*vector_store).await?;
tx.send(Event::SkillIndexed {
    skill_name: skill.manifest.name.clone(),
    skill_version: skill.manifest.version.clone(),
    dims,
    duration_ms: start.elapsed().as_millis() as u64,
}).await.ok();
```

Embedding failure must NOT fail install — log a warning, continue. Rationale: the skill is usable without an embedding (Jaccard path still works); reindex-vec can backfill.

- [ ] **Step 2: `mur skill remove` cleanup**

When `mur skill remove <name>` succeeds, call `skill_index::delete(name, &*vector_store).await.ok()` as a best-effort cleanup. Failure is a logged warning, not an error.

- [ ] **Step 3: `mur skill reindex-vec` command**

```rust
// mur-core/src/cmd/skill_reindex_vec.rs

#[derive(clap::Parser, Debug)]
pub struct ReindexVecArgs {
    /// Optional skill name or glob; reindex all if omitted.
    pub filter: Option<String>,
    /// Remove embeddings for skills no longer installed.
    #[arg(long)]
    pub prune: bool,
}

pub async fn run(args: &ReindexVecArgs, ctx: &Ctx) -> anyhow::Result<()> {
    let installed: Vec<Skill> = list_installed_filtered(&args.filter)?;
    let installed_names: HashSet<String> = installed.iter().map(|s| s.manifest.name.clone()).collect();

    if args.prune {
        let indexed = ctx.vector_store.list_external_ids(SKILL_SOURCE_ID).await?;
        let stale: Vec<String> = indexed.into_iter().filter(|n| !installed_names.contains(n)).collect();
        if !stale.is_empty() {
            ctx.vector_store.delete_by_external_ids(SKILL_SOURCE_ID, &stale).await?;
            println!("Pruned {} stale skill embeddings", stale.len());
        }
    }

    for s in &installed {
        match skill_index::embed_and_upsert(s, &*ctx.embedder, &*ctx.vector_store).await {
            Ok(_)  => println!("Indexed {}", s.manifest.name),
            Err(e) => eprintln!("Failed to index {}: {}", s.manifest.name, e),
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Telemetry event**

`Event::SkillIndexed` added to `mur-agent-runtime/src/telemetry_writer.rs`. Format mirrors `SkillExecuted` from M5a Task 3. Event method name: `mur.skill.indexed`.

- [ ] **Step 5: Build + commit**

```
cargo build -p mur-core -p mur-agent-runtime
git add mur-core/src/cmd/{skill_install.rs,skill_reindex_vec.rs} mur-core/src/main.rs mur-agent-runtime/src/telemetry_writer.rs
git commit -m "feat(skill): auto-embed on install + mur skill reindex-vec"
```

---

### Task 5 — Tests

**Files:** `mur-core/tests/skill_dedup_vec.rs` (new), `mur-core/tests/skill_reindex_vec.rs` (new).

- [ ] **Step 1: Dedup fixture test**

Three skill fixtures:
1. `web-search-google` — manifest text emphasises Google search.
2. `web-search-bing` — same shape, swap Google → Bing. **Expected: NOT flagged as duplicate** (different tool, conceptual overlap only).
3. `web-search-google-v2` — paraphrased description, same tool. **Expected: flagged as duplicate of #1.**
4. `file-organize` — unrelated. **Expected: not flagged.**

Use a deterministic mock embedder for the test (hand-rolled `Embedder` impl that returns hard-coded vectors for the four fixture texts) so the test does not depend on a real embedding model. Assert:
- `report.duplicates.len() == 1`
- The pair is `(web-search-google, web-search-google-v2)` with `source == Vector` and `similarity >= 0.92`.

- [ ] **Step 2: Reindex idempotency test**

Install three skills, run `reindex-vec` twice, assert second run produces the same `chunk_id` set (upsert is idempotent — already guaranteed by `VectorStore::upsert` contract; this test pins the guarantee at the skill-index level).

- [ ] **Step 3: Prune test**

Install A, B, C. Run reindex-vec. Remove B from disk (simulate manual deletion). Run `reindex-vec --prune`. Assert vector store has only A and C.

- [ ] **Step 4: Combined-method test**

Run consolidate with `--method=both` on a fixture where Jaccard and Vector both flag the same pair. Assert the merged `DuplicatePair` has `source == Both` and `similarity` equals the higher of the two scores.

- [ ] **Step 5: Build + commit**

```
cargo test -p mur-core --test skill_dedup_vec --test skill_reindex_vec
git add mur-core/tests/skill_dedup_vec.rs mur-core/tests/skill_reindex_vec.rs
git commit -m "test(skill): vector dedup + reindex-vec fixtures"
```

---

## Operator Documentation

Add a short section to `mur skill consolidate --help` output and to the `docs/architecture/runtime-overview.md` skill section:

```
--method=jaccard   (default) Fast token-set similarity. No embedder required.
--method=vector    Cosine similarity over LanceDB skill embeddings. Catches
                   paraphrased duplicates that Jaccard misses, but requires
                   an embedder configured under `~/.mur/config.yaml`.
--method=both      Run both passes; merged report tags each pair with source.
```

---

## Out of scope — deferred to M6c / M7

1. **LLM adjudication of borderline pairs** (cosine in [0.85, 0.92]) — M6c.
2. **Cross-pattern × skill dedup** (pattern paraphrases a skill or vice versa) — M7.
3. **ANN tuning** — M6c.1 uses LanceDB defaults. Tune only when recall on a real corpus is shown to be insufficient.
4. **Default method flip from Jaccard → Vector** — needs field data; revisit when there's enough.
5. **Embedding model swap-out** — uses whatever `~/.mur/config.yaml` already configures for patterns; one model, two `source_id`s.

## Risks

| Risk | Mitigation |
|---|---|
| Embedder unavailable in CI → install path tries to embed and warns | Already designed: embedding failure is non-fatal at install. Tests use a mock embedder. |
| LanceDB index growth on large registries | Each skill = 1 chunk. 10k skills × 1024 dims × 4 bytes ≈ 40 MB. Acceptable. |
| Reindex-vec on a large registry blocks the CLI | Single-process synchronous loop. Acceptable for v1; surface a progress bar via `indicatif` only if user reports slowness. |
| Threshold tuning is per-embedder-model | Document `COSINE_THRESHOLD` as embedder-dependent; if/when the embedder swaps, M6c.1 may need to re-tune. Track in the constant's doc comment. |
| `source: DedupSource::Jaccard` default on old JSONL reports might confuse downstream parsers | Default annotated with `#[serde(default)]`; old report rows without `source` deserialize as Jaccard, which is correct historically. |

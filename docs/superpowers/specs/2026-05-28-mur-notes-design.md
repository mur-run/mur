# MuR Notes — Personal Knowledge Management with Lifecycle

> **Status:** Draft | **Date:** 2026-05-28 | **Depends on:** Workflow Engine v2
> (`2026-05-28-mur-workflow-engine-design-v2.md`) — Pattern removal **and** the
> shared Skill foundation (Category/ContentMode extension, unified event log,
> stats reducer, `mur skill evolve`).

## Thesis

Pattern as "auto-extracted AI coding rule" never delivered value (`injection_count == 0`
across 160 patterns). But the *infrastructure* — lifecycle state machine, decay,
evidence tracking, hybrid retrieval, linking — is valuable and unique. Nobody in
the 2026 PKM landscape (Karpathy's LLM Wiki, BrainDB, Vannevar, Obsidian
plugins) offers true lifecycle management for personal notes.

**v2 thesis (Workflow Engine):** A workflow is a `Skill` with `category: Workflow`
whose `content.procedure` is executable.

**This thesis (Notes):** A note is a `Skill` with `category: Note` — a
human-curated, file-first knowledge artifact with a managed lifecycle. **Same
on-disk model, same `SkillStats`, same `next_state` lifecycle, same retrieve
pipeline, same `mur skill evolve` sweep.** Workflow and Note are two *projections*
of one knowledge object: Workflow is the *executable* projection (its usage signal
is a run), Note is the *reference* projection (its usage signal is a retrieval).

The only thing Pattern did that Note doesn't is auto-inject into AI coding
sessions (which never worked). What Note adds that Pattern lacked: human
authorship, external-tool interoperability (Obsidian export), and an MCP query
surface.

## Shared foundation (owned by Workflow Engine v2)

Both specs build on **one** model. These elements are defined once in v2 and
**reused, not re-invented, here**:

| Element | Where it lives | Note's use of it |
|---|---|---|
| On-disk unit: `~/.mur/skills/<name>/` directory | v2 storage decision | A note is a skill directory with `category: note` |
| `SkillStats` + its store (`new`/`path`/`load`/`merge_in_place`, **already implemented**) | `skill/stats.rs`, `cmd/skill_stats.rs` | Note reuses the existing persistence; only the event→stats reducer is new |
| `next_state(stats, now)` lifecycle function + `LifecycleState` ladder | `skill/lifecycle.rs` | No new state machine; retrieval stats drive it |
| Per-skill append-only `events.jsonl` → stats reducer | v2 Layer 4 (generalized from `runs.jsonl`) | Note appends a `retrieval` event; workflow appends a `run` event |
| `mur skill evolve [--category …]` sweep | v2 Layer 4 | One sweep evolves notes and workflows alike |
| Corpus vector index (LanceDB) | `store::vector::VectorStore` | Indexes all skills; category is a filter |
| MCP `skill_search(query, category?)` | agent-runtime skill path | Note search is `category: note` filtered |

**Storage decision (route 1, agreed).** The canonical source of truth is the
per-skill directory `~/.mur/skills/<name>/`, **not** a separate `~/.mur/notes/`
markdown tree. This keeps one source of truth, lets the workflow run-ledger live
beside its skill, and reuses the existing skill store. **Obsidian compatibility
becomes a derived export view** (`mur notes export --obsidian <vault>`), not a
second canonical format. We trade "edit `~/.mur/notes/*.md` directly in Obsidian"
(which had no coherent write-back story anyway) for a single, unambiguous source
of truth.

**Note body format (1a — body in `content`).** A note's prose lives **inside**
the canonical `skill.yaml`, in a `content` field (a new `note`/`body` mode →
`ContentMode::Note`), exactly as a context skill stores `content.context` and a
workflow stores `content.procedure`. This is decisive for integrity:
`content_sha256` (the basis of DSSE signing, drift detection, trust hash, and
registry lookup) is computed over the whole manifest's canonical YAML. Body-in-
`content` is covered **for free**; a sibling file would fall outside the signed
hash, breaking the "inherits signing exactly as a Workflow does" promise and
letting a pinned note's prose change without drift detection.

The ergonomic cost of editing YAML-embedded markdown is already solved by the
existing authoring surface: `parser.rs::parse_markdown` round-trips a
markdown-frontmatter view (`---` YAML frontmatter + free markdown body) to/from
canonical `skill.yaml` — its own doc comment states *"markdown is the
human-authoring surface; canonical YAML remains source of truth on disk."* So:

```
~/.mur/skills/rust-error-handling/
├── skill.yaml        ← manifest incl. content.note (the markdown body) — signed/hashed
├── stats.yaml        ← SkillStats (lifecycle_state, usage_count, …)     [shared]
└── events.jsonl      ← append-only usage log (retrieval events)         [shared]
```

- **Edit:** `mur notes edit` renders a SKILL.md view (frontmatter + clean
  markdown), opens `$EDITOR`, re-serializes via `serialize_canonical`.
- **Obsidian export:** `mur notes export --obsidian` emits the same SKILL.md view.
- **Ingest:** `mur notes ingest` parses SKILL.md back via `parse_markdown`.

One round-trip format (SKILL.md) serves editing, export, and ingest; the on-disk
canonical stays single-file `skill.yaml`. (Minor follow-up: force block-scalar
`|` output for multiline strings so `skill.yaml` git diffs stay clean — a shared
serializer nicety, not a blocker, and not new to notes.)

## Market context (2026 Q1–Q2)

Three converging signals validate this direction:

1. **Karpathy's LLM Wiki (April 2026).** File-first three-layer architecture
   (Raw Sources → Wiki → Schema). Core insight: "compile knowledge once at
   ingest time, query the compiled wiki forever." Limitation: `index.md` breaks
   past ~100 pages; community fix is local hybrid search (llmwiki: Tantivy BM25
   + fastembed vectors + RRF).

2. **"Memory as Metabolism" (arXiv:2604.12034, April 2026).** Five core
   operations for personal LLM memory: TRIAGE, DECAY, CONTEXTUALIZE,
   CONSOLIDATE, AUDIT. Identifies "Kuhnian ossification" — wikis entrench
   dominant interpretations — as the key failure mode. mur's lifecycle state
   machine + decay + evidence tracking is the only open-source implementation
   that maps to all five operations.

3. **BrainDB (2026).** 5,420+ memories in production. SQLite + FTS5 + vectors.
   30-day half-life freshness scoring. Contradiction detection. Multi-agent
   coordination. Closest real-world analogue to mur Notes — but no lifecycle
   state machine (decay is search-ranking only, no promote/demote/archive).

**Gap:** No competitor has maturity lifecycle (Draft → Emerging → Stable →
Canonical → Deprecated → Archived). This is mur's differentiator — and because a
Note *is* a Skill, it inherits that lifecycle, hybrid retrieval, linking, signing,
and peer sharing for free, exactly as a Workflow does.

## Architecture

```
~/.mur/skills/                       ← Source of truth (one directory per skill)
├── rust-error-handling/             ← category: note
│   ├── skill.yaml                   ← incl. content.note (markdown body)
│   ├── stats.yaml
│   └── events.jsonl
├── fly-io-deploy/                   ← category: workflow (v2)
│   ├── skill.yaml
│   ├── stats.yaml
│   ├── events.jsonl                 ← run events
│   └── runs.jsonl                   ← (v2 projection of events; see v2 spec)
└── ...

        ↓ parse + embed (rebuildable)

┌─────────────────────────────┐
│ LanceDB (~/.mur/vectors/)   │  ← corpus vector index: hybrid BM25 + semantic
│                             │     rebuildable from skill directories
└─────────────────────────────┘

   (optional, deferred) SQLite corpus metadata index — see Storage layers

        ↓ expose via

┌──────────┬──────────┬───────────────────────┐
│ MCP      │ CLI      │ Obsidian export view  │
│ skill_*  │ mur notes│ (derived, not source) │
└──────────┴──────────┴───────────────────────┘
```

**Design rules:**
- The per-skill directory is canonical. LanceDB (and any SQLite index) are
  derivative — delete either, rebuild from skill directories via `mur skill reindex`.
- One source of truth. Obsidian interop is a *generated export*, never a second
  canonical store.
- No schema migrations on derivative indexes. Schema change → delete index, rescan.

## Data model

A note reuses the existing `SkillManifest` with two foundation extensions
(defined in v2, listed here for completeness):

1. `Category` gains a `Note` variant (currently `Context | Workflow | Command | Meta`).
2. `ContentMode` gains a `Note` variant (currently `Context | Workflow | Command`);
   `Content::mode()` returns `Note` when the note body is present.

### On-disk `skill.yaml` (note-mode)

```yaml
schema: <skill manifest schema version>
name: rust-error-handling           # kebab-case, unique id
description: Rust 錯誤處理最佳實踐     # one-line summary (also the L2 abstract)
category: note
kind: reference                      # note-specific sub-kind (see NoteKind)
tags: [rust, error-handling, patterns]
tier: project
importance: 0.8
links:
  related: [rust-async-patterns]
  supersedes: []
created_at: 2026-05-28T10:00:00Z
updated_at: 2026-05-28T14:00:00Z
content:
  abstract: Rust 錯誤處理最佳實踐
  note: |                            # markdown body lives here → ContentMode::Note
    # Rust Error Handling

    ## Technical
    使用 `anyhow` 處理應用層錯誤,`thiserror` 定義 library 錯誤類型。

    ## Principle
    不要在 library 層 leak `anyhow::Error`——呼叫方無法 match。
# lifecycle_state / decay / usage live in stats.yaml, NOT here (manifest is signable & immutable)
```

### SKILL.md authoring/export view (derived, via `parse_markdown`)

```markdown
---
name: rust-error-handling
category: note
kind: reference
tags: [rust, error-handling, patterns]
---

# Rust Error Handling

## Technical
使用 `anyhow` 處理應用層錯誤,`thiserror` 定義 library 錯誤類型。

## Principle
不要在 library 層 leak `anyhow::Error`——呼叫方無法 match。
```

`mur notes edit` / `export` / `ingest` all use this round-trip view; the on-disk
canonical remains the single-file `skill.yaml` above.

### Rust types

`kind` is the only note-specific manifest field. Everything else is shared
`SkillManifest`.

```rust
// mur-common/src/skill/manifest.rs — add to the manifest
#[serde(default, skip_serializing_if = "Option::is_none")]
pub kind: Option<NoteKind>,          // only meaningful for category: note

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    #[default]
    Reference,    // how-to, best practice, documentation
    Decision,     // architectural decision record
    Fact,         // server address, config value, known limitation
    Procedure,    // step-by-step (may graduate to a category: workflow skill)
    Insight,      // observation, analysis, learned lesson
}
```

The markdown body is stored in `content.note` inside `skill.yaml` (note mode of
`Content`). The SKILL.md view is a derived authoring/export form, not a second
canonical file.

### What `category: note` shares vs. specializes

Shares with every skill (incl. `category: workflow`):
- `SkillStats` lifecycle state machine (`skill/stats.rs`, `skill/lifecycle.rs`)
- Decay (`Tier` half-lives), linking (`links.related`, `links.supersedes`)
- Evidence via the shared `events.jsonl` reducer
- Vector indexing (LanceDB), MCP exposure, signing, registry, peer transfer

Specializes:
- Content is free markdown (`content.note`), not a structured `procedure` DAG
- Carries `kind: NoteKind`; workflows carry `on_failure` / `retry` per step
- Usage signal is a **retrieval** event, not a **run** event

### What we keep from Pattern (route 1)

| Component | Keep? | Notes |
|---|---|---|
| `KnowledgeBase` fields (name, desc, tier, importance, tags, links, created_at) | Fold into `SkillManifest` | Drop `PatternKind` (→ `NoteKind`), drop `confidence` (human-authored, not extracted — make it `Option`, left empty for notes), drop `Content::DualLayer` (free markdown in `content.note`) |
| `Tier` + half-lives | Keep | session=14d, project=90d, core=365d (shared, exposed in config) |
| `Maturity` / `LifecycleState` | Keep | Reuse `skill/lifecycle.rs` ladder verbatim |
| `Evidence` | Replace | Superseded by `SkillStats` + `events.jsonl` (retrieval signal) |
| `Links` | Keep | related, supersedes |
| `capture/noise_filter.rs` | Keep | Repurposed for local-LLM ingestion preprocessing (shared with v2 extraction) |
| `retrieve/` (scoring, gate, unified) | **Adapt** (not just keep) | The hybrid BM25+vector logic is reusable, but it is hard-typed to `Pattern` today (`ScoredPattern { pattern: Pattern }`, `score_and_rank_hybrid(Vec<Pattern>)`). Introduce a `Retrievable` trait (name, description, embed text, tier, importance, decay inputs) so Skill/Note and the transitional Pattern share one scorer. This is the most under-estimated migration in both specs — a type migration, not a repoint. |
| `store/yaml.rs` | Reuse | The existing skill store writes `skill.yaml` with the body in `content.note` — no new file I/O, no `body.md` |
| `store/lancedb.rs` | Keep | Corpus vector index, rebuildable |
| `inject/hook.rs` | Replace | Auto-inject → MCP `skill_search` + CLI (shared with v2) |
| `evolve/decay.rs`, `evolve/maturity.rs`, `evolve/lifecycle.rs` | Drop | Superseded by `skill/lifecycle.rs` + `mur skill evolve` |
| `capture/emergence.rs`, `capture/feedback.rs` | **Remove** | LLM auto-extraction + injection feedback (no injection = no feedback loop) |
| `inject/` (event, index, queue, stats) | **Remove** | Passive query, not active push |
| `Pattern` type | **Remove** | Replaced by `category: note` skills |

## Storage layers

### Layer 1: Filesystem (source of truth)

- Path: `~/.mur/skills/<name>/` (one directory per skill, all categories).
- Note-mode keeps its markdown body in `content.note` inside `skill.yaml`; the
  SKILL.md authoring/export view is derived via `parse_markdown`.
- `tags` and an optional `path:`/topic field in the manifest carry the taxonomy
  (route 1 has no directory-as-category tree — the corpus is flat under
  `~/.mur/skills/`; grouping is by tag/topic, surfaced in CLI and the export view).
- Atomic writes: temp file + rename (existing skill store behaviour).
- Git-friendly: `skill.yaml` is plain text, diffable, mergeable (force block-scalar
  output for the body keeps prose diffs clean).

### Layer 2: SQLite corpus metadata index (optional, deferred)

- **Not in the MVP.** At current corpus size (a handful of skills, ~160 patterns
  to migrate) a directory scan is fast enough. Per Mandatory Rule #1, do not add
  a second index until it earns its place.
- When added, it indexes the **whole skill corpus** keyed by category
  (`~/.mur/skills.db`), not a notes-only table. Purpose: fast `WHERE` filtering
  (`category = note AND maturity = stable`) without parsing every directory.
- Rebuild: `mur skill reindex`. No migration: `PRAGMA user_version` mismatch →
  delete and rebuild.

### Layer 3: LanceDB (vector index)

- Existing `store::vector::VectorStore` trait; `LanceDbStore` impl; corpus-wide.
- Embedding: reuse the existing configurable embedder (`store/embedding.rs`):
  provider + model come from `config.embedding` (Ollama default; current default
  model `qwen3-embedding:0.6b`). **Do not hardcode a model** (Mandatory Rule #1).
- **Dimension consistency:** notes MUST embed with the same model/dimensions as
  the rest of the skill corpus, or hybrid search across categories breaks.
  Changing the model is a corpus-wide reindex, never a notes-only change.
- Embedding input (note): `name + "\n" + description + "\n" + body` (truncated per
  `config.embedding` limit).
- Hybrid scoring: vector similarity (0.7) + BM25 keyword (0.3), fused via RRF.
- Max results: 10. Recency boost: ×1.2 for skills active in last 7 days.

## Retrieval

### CLI (`mur notes` = `category: note` convenience facade over `mur skill`)

```
mur notes search "rust error handling"   # = mur skill search --category note
mur notes show <name>
mur notes list --kind decision --maturity stable
mur notes edit <name>            # SKILL.md view in $EDITOR, round-trips to skill.yaml
mur notes create --kind reference
mur notes archive <name>
mur notes export --obsidian <vault>      # derived flat-markdown view
mur skill reindex                        # rebuild LanceDB (+ SQLite if enabled) — shared
mur skill evolve --category note         # lifecycle sweep — shared
```

`mur notes` is a thin facade; reindex and evolve are **shared** `mur skill`
commands, not note-private, so notes and workflows evolve through one path.

### MCP server (unified surface)

Expose one search surface with a category filter, plus note-create:

```
mcp__mur__skill_search(query, category?, kind?, maturity?, limit?)
mcp__mur__skill_read(name)
mcp__mur__note_create(name, description, kind, body, tags?)
mcp__mur__note_link(from, to, relationship)
```

`notes_search` / `notes_read` may exist as thin aliases that pin
`category: note`, but the canonical tools are category-filtered `skill_*`. The
server is a thin wrapper around the existing agent-runtime skill path — notes are
just `category: note` skills.

### Obsidian / external tools (derived export)

`mur notes export --obsidian <vault>` writes flat `SKILL.md` files
(frontmatter + body) into the target vault. The export is read-oriented: edits
made in Obsidian are not written back to `~/.mur/skills/` automatically. To bring
external edits in, `mur notes ingest <file>` (post-MVP, see below) re-imports a
markdown file as a note. mur does not compete with Obsidian — it adds a lifecycle
layer and an AI query surface on top of the same content, exposed as a vault view.

## Lifecycle (the differentiator)

Reuse `skill/lifecycle.rs` and `SkillStats` unchanged. The note-specific part is
only *what counts as a use*: a **retrieval** (CLI/MCP read) appends a
`{"ts": …, "kind": "retrieval"}` line to the skill's shared `events.jsonl`. The
stats reducer turns that into `usage_count` / `last_success_at`, and
`next_state(stats, now)` does the rest — the same ladder workflows use.

| State | Note condition (via shared ladder) |
|---|---|
| **Draft** | Default. Newly created, not yet validated. |
| **Emerging** | Retrieved ≥ `PROMOTE_DRAFT_USES` (default 3). |
| **Stable** | Retrieved ≥ `PROMOTE_EMERGING_USES` (default 10), rate/age gates met. |
| **Canonical** | `pinned` by user. Never auto-demoted. |
| **Deprecated** | No retrieval in 90d, OR user explicitly deprecates. |
| **Archived** | Decay + age past threshold. Candidate for hard-delete sweep. |

**Retrieval has no failure.** A retrieval always "succeeds", so for a note every
event increments both `usage_count` and `success_count` (`success_rate == 1.0`).
The promotion ladder therefore degenerates to **count + age** (the rate gates
auto-pass), and the `success_rate < 0.3` deprecation rule **never fires** for
notes — note demotion is driven only by decay+age (`AUTO_ARCHIVE_*`) or manual
deprecate. This is intentional; it must be explicit so an implementer does not
invent a "note failure" signal. `SkillStats::anchor_confidence` (the decaying
quantity, default 1.0) is seeded from the note's `importance` at create time.

**Why append-only events, not write-on-read.** The first draft mutated
`last_active` on every retrieval. Appending to `events.jsonl` instead avoids
read-path write contention, matches the workflow run-ledger exactly, and lets
the shared reducer compute stats lazily during `mur skill evolve`.

**Decay:** Tier half-lives (session=14d, project=90d, core=365d) determine the
rate; `importance` decays by `0.5 ^ (days_since_last_active / half_life_days)`.
Thresholds live in config (shared with v2 P4).

**Contradiction detection** (post-MVP): optional nightly sweep via local LLM.
Two notes with high similarity but conflicting conclusions → flag for user
review. Inspired by BrainDB's contradiction strategies.

## Local LLM integration (post-MVP)

A local LLM (Ollama, 7B+) can assist with:

1. **Ingest:** `mur notes ingest <file>` — reads a raw document (or an
   Obsidian-edited export), extracts structured notes with frontmatter, writes
   them as `category: note` skills.
2. **Consolidate:** detects near-duplicate notes, suggests merges.
3. **Contradiction detection:** nightly sweep for conflicting claims.
4. **Summarize:** `mur notes summarize <name>` — regenerates `description` from body.

All LLM features are **optional**. Manual `mur notes create` requires no LLM.
Gate: if `ollama list` returns no models, LLM features are disabled with a clear
message ("install Ollama and pull a model to enable AI features").

`source_sessions` (which sessions a note was extracted from) is **only** populated
by the LLM ingest path. Since ingest is post-MVP, the field is omitted from the
MVP manifest and added with the ingest feature, rather than carried as a dead
field.

## Migration from Patterns

Aligned with v2's Pattern-removal sequence (v2 owns the removal; this owns the
note bootstrap):

1. v2 exports all 160 patterns to `~/.mur/exported-patterns/<name>.md` (markdown
   with frontmatter) before deleting `~/.mur/patterns/` and the pattern pipeline.
2. Users manually promote exported patterns worth keeping by importing them as
   notes: `mur notes ingest ~/.mur/exported-patterns/rust-error-handling.md`
   (or, pre-ingest, hand-create the `~/.mur/skills/<name>/` directory).
3. `mur skill reindex` rebuilds the LanceDB index.

No automatic migration. The old patterns are code-convention-focused and most
have low value as general reference notes — let the user curate.

## Development phases

P1–P4 deliver a working CLI note system without LLM dependency. They depend on
the **shared foundation** landing first (v2 P1–P4: Pattern removal, Category/
ContentMode extension, `events.jsonl` + stats reducer, `mur skill evolve`).

| Phase | Content | Est. | Depends on |
|---|---|---|---|
| **N1: Note schema** | `category: note`, `ContentMode::Note`, `content.note` body field, `NoteKind`; extend `parse_markdown`/`serialize_canonical` for the note SKILL.md round-trip (no new file I/O) | 3–4 d | v2 Category/ContentMode + skill store |
| **N2: CLI facade** | `mur notes {search, show, list, create, edit, archive}` over `mur skill`; `mur notes export --obsidian` | 1 wk | N1, v2 corpus index |
| **N3: Retrieve** | Implement `Retrievable` for `Note`; reuse hybrid retrieval (LanceDB + BM25 + RRF); recency/importance weighting | 2–3 d | N1, v2 `Retrievable` trait (P1b) |
| **N4: Lifecycle wire-up** | Retrieval → `events.jsonl`; rely on shared stats reducer + `next_state` + `mur skill evolve --category note` | 2–3 d | v2 events/stats/evolve |
| **N5: MCP** | `skill_search(category?)`, `skill_read`, `note_create`, `note_link` via agent-runtime skill path | 3–4 d | v2 MCP skill path |
| **N6: Local LLM (P2)** | ingest, consolidate, contradiction detection, summarize — gated on Ollama | 1 wk | N1–N5 |

Total incremental over the shared foundation: ~3.5 weeks to full feature set.

## Resolved decisions

1. **Route 1 storage (agreed).** Canonical source of truth is the per-skill
   directory `~/.mur/skills/<name>/`. No separate `~/.mur/notes/` tree. Obsidian
   is a derived export view, not a second canonical format.
2. **Note body lives in `content.note` inside `skill.yaml`** (route 1a), not a
   sibling file. Decisive reason: `content_sha256` (DSSE signing, drift detection,
   trust hash, registry lookup) hashes the whole manifest's canonical YAML — body-
   in-`content` is signed/drift-covered for free, identical to a workflow
   `procedure`; a sibling file falls outside the hash. Authoring ergonomics come
   from the existing `parse_markdown` SKILL.md round-trip, not from a second
   canonical file.
3. **No automatic injection.** Notes are passive reference material queried on
   demand via CLI or MCP. The `inject/` push modules are removed.
4. **Lifecycle is shared, not re-implemented.** Notes reuse `SkillStats` +
   `skill/lifecycle.rs` + the shared `events.jsonl` reducer + `mur skill evolve`.
   A note "use" is a retrieval event; a workflow "use" is a run event.
5. **`mur notes` is a facade.** Category-private convenience commands wrap shared
   `mur skill` machinery (reindex/evolve are shared, not note-private).
6. **Unified MCP surface.** `skill_search(category?)` is canonical; `notes_*` are
   optional aliases.
7. **SQLite metadata index is deferred** and, when added, is corpus-wide
   (all categories), not notes-only.
8. **`confidence` becomes `Option`, left empty for notes** (extraction artifact,
   meaningful only for v2's extracted workflows).
9. **`source_sessions` ships with the LLM ingest feature**, not the MVP.
10. **No automatic migration from Patterns.** v2 exports as archive; user curates
    what to promote via `mur notes ingest`.
11. **Notes and Workflows are both Skills** (`category: note` / `category:
    workflow`) — two projections of one knowledge object, sharing lifecycle,
    decay, linking, retrieval, signing, peer transfer, and MCP exposure, differing
    only in content shape (free markdown vs. structured DAG) and usage signal
    (retrieval vs. run).

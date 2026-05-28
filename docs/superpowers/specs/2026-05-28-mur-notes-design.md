# MuR Notes — Personal Knowledge Management with Lifecycle

> **Status:** Draft | **Date:** 2026-05-28 | **Depends on:** Workflow Engine v2 (pattern removal)

## Thesis

Pattern as "auto-extracted AI coding rule" never delivered value (`injection_count == 0`
across 160 patterns). But the *infrastructure* — lifecycle state machine, decay,
evidence tracking, hybrid retrieval, linking — is valuable and unique. Nobody in
the 2026 PKM landscape (Karpathy's LLM Wiki, BrainDB, Vannevar, Obsidian
plugins) offers true lifecycle management for personal notes.

**v2 thesis (Workflow Engine):** A workflow is a Skill with `category: Workflow`.

**This thesis (Notes):** A note is a Skill with `category: Note` — a
human-curated, file-first knowledge artifact with a managed lifecycle. Same
subsystem, same retrieve pipeline, same decay and evolution. The only thing
Pattern did that Note doesn't is auto-inject into AI coding sessions (which
never worked). What Note adds that Pattern lacked: human authorship, external
tool interoperability, and an MCP query surface.

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
Canonical → Deprecated → Archived). This is mur's differentiator.

## Architecture

```
~/.mur/notes/                    ← Source of truth (markdown + YAML frontmatter)
├── rust/
│   ├── error-handling.md
│   └── async-patterns.md
├── deploy/
│   └── fly-io-setup.md
└── ...

        ↓ parse + embed (rebuildable)

┌─────────────────────────────┐
│ SQLite (~/.mur/notes.db)    │  ← metadata index: tags, path, maturity, decay, mtime
│                             │     fast filtering: "rust + stable, not deprecated"
└─────────────────────────────┘

┌─────────────────────────────┐
│ LanceDB (~/.mur/vectors/)   │  ← vector index: hybrid BM25 + semantic search
│                             │     rebuildable from markdown files
└─────────────────────────────┘

        ↓ expose via

┌──────────┬──────────┬──────────┐
│ MCP      │ CLI      │ Obsidian │
│ server   │ search   │ vault    │
└──────────┴──────────┴──────────┘
```

**Design rules:**
- Files are the canonical source. SQLite and LanceDB are derivative indexes —
  delete either, rebuild from files.
- No schema migrations on SQLite. Schema changes → delete `.db`, rescan.
- Obsidian-compatible: drop `~/.mur/notes/` into any Obsidian vault. Extra
  YAML frontmatter fields are ignored by Obsidian but preserved.

## Data model

### On-disk format: Markdown + YAML frontmatter

```markdown
---
schema: 4
name: rust-error-handling
description: Rust 錯誤處理最佳實踐
kind: reference
tags: [rust, error-handling, patterns]
tier: project
importance: 0.8
maturity: stable
created_at: 2026-05-28T10:00:00Z
updated_at: 2026-05-28T14:00:00Z
decay:
  last_active: 2026-05-28T14:00:00Z
links:
  related: [rust-async-patterns]
  supersedes: []
source_sessions: [session-abc123]
---

# Rust Error Handling

## Technical
使用 `anyhow` 處理應用層錯誤，`thiserror` 定義 library 錯誤類型。

## Principle
不要在 library 層 leak `anyhow::Error`——呼叫方無法 match。
```

### Rust struct (in-memory + parse)

```rust
// mur-common/src/note.rs
pub struct NoteManifest {
    pub schema: u32,
    pub name: String,                    // kebab-case, unique id
    pub description: String,             // one-line summary
    #[serde(default)]
    pub kind: NoteKind,                  // reference | decision | fact | procedure | insight
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tier: Tier,                      // session | project | core (reused)
    #[serde(default = "default_importance")]
    pub importance: f64,
    #[serde(default)]
    pub maturity: Maturity,              // Draft | Emerging | Stable | Canonical | Deprecated | Archived
    pub created_at: DateTime<Utc>,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub decay: DecayMeta,
    #[serde(default)]
    pub links: NoteLinks,
    #[serde(default)]
    pub source_sessions: Vec<String>,    // only populated when ingested by local LLM
    // ── Body: everything after the `---` frontmatter fence, stored separately ──
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    #[default]
    Reference,    // how-to, best practice, documentation
    Decision,     // architectural decision record
    Fact,         // server address, config value, known limitation
    Procedure,    // step-by-step (may graduate to a Workflow skill)
    Insight,      // observation, analysis, learned lesson
}

pub struct Note {
    pub manifest: NoteManifest,
    pub body: String,                    // everything after the frontmatter
}
```

### Relationship to Skill

A `Note` and a `Workflow` are both `Skill` variants with different `category`
values. They share:
- Lifecycle state machine (`skill/lifecycle.rs`)
- Decay (`Tier` half-lives)
- Linking (`links.related`, `links.supersedes`)
- Evidence tracking (usage count, success rate)
- Vector indexing (LanceDB)
- MCP exposure (agent-runtime skill path)

They differ in:
- Note has no `procedure` (it's reference, not executable)
- Note has `NoteKind`; Workflow has `FailureAction`/`RetryConfig`
- Note body is free markdown; Workflow procedure is structured DAG steps

### What we keep from Pattern

| Component | Keep? | Notes |
|---|---|---|
| `KnowledgeBase` (name, desc, tier, importance, tags, applies, evidence, links, lifecycle, decay, maturity, scope, created_at) | Repurpose | Slim down to `NoteManifest` — drop `PatternKind` (replace with `NoteKind`), drop `confidence` (human-authored, not extracted), drop `Content::DualLayer` (free markdown instead) |
| `Tier` + half-lives | Keep | session=14d, project=90d, core=365d |
| `Maturity` + `LifecycleState` | Keep | Draft→Emerging→Stable→Canonical→Deprecated→Archived |
| `Evidence` | Keep (simplified) | `retrieval_count`, `last_retrieved_at` instead of injection signals |
| `Links` | Keep | related, supersedes |
| `DecayMeta` | Keep | last_active, half_life_override |
| `capture/noise_filter.rs` | Keep | Repurposed for local LLM ingestion preprocessing |
| `retrieve/` (scoring, gate, unified) | Keep | Hybrid BM25 + vector, recency boost, importance weighting |
| `store/yaml.rs` | Replace | YAML → Markdown + frontmatter parser |
| `store/lancedb.rs` | Keep | Vector index, rebuildable |
| `inject/hook.rs` | Replace | Auto-inject → MCP server + CLI search |
| `evolve/decay.rs`, `evolve/maturity.rs`, `evolve/lifecycle.rs` | Keep | Adapted to note retrieval signals |
| `capture/emergence.rs` | **Remove** | LLM auto-extraction from sessions |
| `capture/feedback.rs` | **Remove** | No injection = no feedback loop |
| `inject/` (event, index, queue, stats) | **Remove** | Passive query, not active push |
| `Pattern` type | **Remove** | Replaced by `Note` |

## Storage layers

### Layer 1: Filesystem (source of truth)

- Path: `~/.mur/notes/<path-segments>/<name>.md`
- Directory structure IS the category taxonomy. `rust/error-handling.md` has
  implicit path `rust`.
- Atomic writes: temp file + rename (same as current YAML store).
- Git-friendly: plain text, diffable, mergeable.

### Layer 2: SQLite (metadata index)

- Path: `~/.mur/notes.db`
- Schema: one table `notes` with columns for every filterable field
  (name, path, kind, tier, maturity, importance, tags as JSON array,
  created_at, updated_at, decay_last_active).
- Purpose: fast `WHERE` queries without parsing 200 markdown files.
- Rebuild: `mur notes reindex` — scans `~/.mur/notes/`, parses frontmatter,
  upserts rows.
- No migration: schema version stored as `PRAGMA user_version`. On mismatch,
  delete and rebuild.

### Layer 3: LanceDB (vector index)

- Existing `store::vector::VectorStore` trait; `LanceDbStore` impl.
- Embedding model: `nomic-embed-text-v1.5` via Ollama (local, free).
- Embedding input: `name + "\n" + description + "\n" + body` (truncated to
  512 tokens).
- Hybrid scoring: vector similarity (0.7) + BM25 keyword (0.3), fused via RRF.
- Max results: 10. Recency boost: ×1.2 for notes active in last 7 days.

## Retrieval

### CLI

```
mur notes search "rust error handling"
mur notes show <name>
mur notes list --kind decision --maturity stable
mur notes edit <name>            # opens $EDITOR
mur notes create --kind reference
mur notes archive <name>
mur notes reindex                # rebuild SQLite + LanceDB from files
```

### MCP server

Expose as MCP tools so AI assistants (Claude Code, Cursor, etc.) can search,
read, and create notes during sessions:

```
mcp__mur__notes_search(query, kind?, maturity?, limit?)
mcp__mur__notes_read(name)
mcp__mur__notes_create(name, description, kind, body, tags?)
mcp__mur__notes_link(from, to, relationship)
```

The MCP server is a thin wrapper around the existing agent-runtime skill
injection path — notes are just `category: Note` skills.

### Obsidian / external tools

`~/.mur/notes/` is a valid Obsidian vault root. Users can:
- Open it directly as an Obsidian vault
- Symlink it into an existing vault
- Use any markdown editor

mur does not compete with Obsidian — it adds a lifecycle layer and an AI query
surface *on top of* the same markdown files.

## Lifecycle (the differentiator)

Reuse `skill/lifecycle.rs` with note-specific signals:

| State | Condition |
|---|---|
| **Draft** | Default. Newly created, not yet validated. |
| **Emerging** | Retrieved ≥ 3 times. Promising. |
| **Stable** | Retrieved ≥ 10 times, last active < 30d. Trusted. |
| **Canonical** | Pinned by user. Never auto-demoted. |
| **Deprecated** | No retrieval in 90d, OR user explicitly deprecates. |
| **Archived** | Deprecated + 180d no activity. Candidate for deletion. |

**Decay:** `last_active` is updated on every retrieval. Tier half-lives
determine decay rate (session=14d, project=90d, core=365d). Importance decays
by `0.5 ^ (days_since_last_active / half_life_days)`.

**Promotion/demotion sweep:** `mur notes evolve` (runnable manually or via
cron). Sweeps all notes, updates maturity based on retrieval stats, decays
importance, flags candidates for archival.

**Contradiction detection** (post-MVP): optional nightly sweep via local LLM.
Two notes with high similarity but conflicting conclusions → flag for user
review. Inspired by BrainDB's five contradiction strategies.

## Local LLM integration (post-MVP)

A local LLM (Ollama, 7B+) can assist with:

1. **Ingest:** `mur notes ingest <file>` — reads a raw document, extracts
   structured notes with frontmatter, writes them into `~/.mur/notes/`.
2. **Consolidate:** detects near-duplicate notes, suggests merges.
3. **Contradiction detection:** nightly sweep for conflicting claims.
4. **Summarize:** `mur notes summarize <name>` — regenerates description from body.

All LLM features are **optional**. Manual `mur notes create` requires no LLM.
The gate is: if `ollama list` returns no models, LLM features are disabled
with a clear message ("install Ollama and pull a model to enable AI features").

## Migration from Patterns

1. Export all 160 patterns to `~/.mur/exported-patterns/<name>.md` (markdown
   with frontmatter).
2. Delete `~/.mur/patterns/`, `~/.mur/fingerprints.jsonl`.
3. Remove pattern pipeline code (emergence, fingerprinting, feedback, inject).
4. Create `~/.mur/notes/` directory with a README.md explaining the new system.
5. Users manually promote exported patterns they want to keep:
   `cp ~/.mur/exported-patterns/rust-error-handling.md ~/.mur/notes/rust/`
6. Run `mur notes reindex` to build SQLite + LanceDB indexes.

No automatic migration. The old patterns are code-convention-focused and most
have low value as general reference notes. Let the user curate.

## Development phases

| Phase | Content | Est. |
|---|---|---|
| **P1: Schema + storage** | `NoteManifest` struct; markdown frontmatter parser; SQLite metadata index; file watcher (mtime → reindex changed files) | 1 wk |
| **P2: CLI** | `mur notes {search, show, list, create, edit, archive, reindex, evolve}` | 1 wk |
| **P3: Retrieve** | Adapt existing hybrid retrieval (LanceDB + BM25) for notes; RRF fusion; recency/importance weighting | 3–4 d |
| **P4: Lifecycle** | Adapt `skill/lifecycle.rs` for note retrieval signals; `mur notes evolve` sweep; decay + promotion/demotion | 3–4 d |
| **P5: MCP server** | Expose `notes_search`, `notes_read`, `notes_create`, `notes_link` as MCP tools via agent-runtime skill path | 3–4 d |
| **P6: Local LLM (P2)** | Ingest, consolidate, contradiction detection, summarize — all gated on Ollama availability | 1 wk |
| **P7: Migration** | Pattern export → md; pattern pipeline removal (from Workflow Engine v2 P1); `~/.mur/notes/` bootstrap | 1–2 d |

Total: ~5 weeks to full feature set. P1–P4 deliver a working CLI note system
without LLM dependency. P5 adds AI assistant integration. P6 adds LLM smarts.

## Resolved decisions

1. **Markdown + YAML frontmatter is the canonical format.** Not SQLite, not
   plain YAML. Markdown is human-readable, Obsidian-compatible, git-diffable.
2. **SQLite is a derivative index, not source of truth.** Delete and rebuild
   from files at any time.
3. **No automatic injection.** Notes are passive reference material queried on
   demand via CLI or MCP. The `inject/` module is removed entirely.
4. **Local LLM is optional.** All core features work without one. LLM features
   are gated on Ollama availability.
5. **No automatic migration from Patterns.** Export as archive; user curates
   what to promote.
6. **Notes and Workflows are both Skills** (`category: Note` / `category:
   Workflow`). They share lifecycle, decay, linking, retrieval, and MCP
   exposure. They differ in content shape (free markdown vs. structured DAG).
7. **Tier half-lives and maturity ladder are exposed in config** (per Mandatory
   Rule #1, shared with Workflow Engine v2 P4).

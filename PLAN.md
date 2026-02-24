# MUR Core v2.0 — Full Rust Rewrite Plan

> Version: 1.0
> Date: 2026-02-25
> Status: Approved
> Author: David + Claude (architecture session)

## Executive Summary

MUR Core v2 is a complete rewrite from Go to Rust, transforming Mur from a "note-taking tool" into a "learning brain" for AI assistants. The rewrite introduces semantic retrieval (LanceDB), pattern evolution (feedback-driven lifecycle), and a multi-signal scoring pipeline — while preserving Mur's core differentiator: human-readable, git-friendly YAML patterns.

**Why rewrite instead of iterate:**
- v2 architecture changes are so fundamental (retrieve engine, evolve engine, LanceDB integration) that >60% of Go code would need rewriting anyway
- Rust has native LanceDB support (LanceDB is written in Rust)
- Cargo workspace enables shared crates with MUR Commander (also Rust)
- Single language, single binary, zero FFI overhead

**Timeline:** 8 weeks full-time (with AI assistance)
**Lines of code:** ~7,500 Rust (4,500 rewrite + 3,000 new features)

---

## 1. Current State Diagnosis (v1 Go)

```
207 patterns, 0 injections tracked, Avg Effectiveness 50%
1/8 sync targets, 213 YAML files
```

### What's valuable (KEEP)
1. **Local-first YAML patterns** — human-readable, vim-editable, git-friendly. Core differentiator vs Mem0/Zep black boxes.
2. **Hook mechanism** — integrated with Claude Code, Gemini, Auggie. Zero-friction injection.
3. **CLI philosophy** — `mur new`, `mur search`, `mur learn`. Developer-friendly.
4. **Cross-tool sync** — one pattern syncs to 10+ tools.

### What's broken (FIX)
1. **No retrieval intelligence** — full load or tag match, no semantic ranking
2. **No lifecycle** — patterns live forever, no decay, no eviction
3. **No feedback loop** — no tracking of whether patterns are used or effective
4. **Low extraction quality** — `mur learn extract` has no noise filter, junk patterns accumulate
5. **No pattern relationships** — flat YAML files, no links between related patterns

---

## 2. Research Foundations

| Source | Key Concept | Impact on Mur v2 |
|--------|-------------|-------------------|
| **A-Mem** (NeurIPS 2025) | Zettelkasten dynamic linking — new memories trigger old memory updates | Pattern linking graph |
| **Mem0** (arXiv 2025) | Dynamic extraction + consolidation + retrieval, 91% low-latency | Smart token budget |
| **MemoryOS** (EMNLP 2025) | STM → MTM → LPM three-tier, heat-based eviction | Pattern tiers: session/project/core |
| **Zep/Graphiti** (2025) | Temporal knowledge graph, bitemporal modeling | Time-aware pattern scoring |
| **EvoMap GEP** | Gene + Capsule + Evolver + GDI ranking | Evidence-based evolution |
| **memory-lancedb-pro** | 8-stage retrieval pipeline, noise filter, adaptive retrieval | Multi-stage scoring pipeline |
| **LanceDB Pro Plugin Rules** | Dual-layer storage, atomic entries <500 chars, recall-before-retry, dedup | Capture pipeline quality gates |

---

## 3. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│ MUR Core v2 (Rust)                                                  │
│                                                                     │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────────┐   │
│  │ CAPTURE  │─→│  STORE   │─→│ RETRIEVE │─→│     INJECT       │   │
│  │ Pipeline │  │  Engine  │  │  Engine  │  │    Runtime       │   │
│  └──────────┘  └──────────┘  └──────────┘  └──────────────────┘   │
│       │             │             │                   │             │
│       │             ▼             │                   │             │
│       │        ┌──────────┐      │                   │             │
│       │        │  EVOLVE  │──────┘                   │             │
│       │        │  Engine  │◄─────────────────────────┘             │
│       │        └──────────┘                                        │
│       │             │                                              │
│       ▼             ▼                                              │
│  ┌──────────────────────────────────────┐                          │
│  │         Pattern Graph                │                          │
│  │  (YAML + LanceDB Index + Links)      │                          │
│  └──────────────────────────────────────┘                          │
│                     │                                              │
│         ┌───────────┼───────────┐                                  │
│         ▼           ▼           ▼                                  │
│    ┌─────────┐ ┌──────────┐ ┌──────────┐                          │
│    │  LOCAL  │ │  CLOUD   │ │COMMUNITY │                          │
│    │ ~/.mur/ │ │ mur.run  │ │ GEP Hub  │                          │
│    └─────────┘ └──────────┘ └──────────┘                          │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 4. Project Structure

### 4.1 Cargo Workspace (shared with MUR Commander)

```
~/Projects/mur-workspace/
├── Cargo.toml              # [workspace]
├── mur-common/             # shared types & traits
│   └── src/
│       ├── lib.rs
│       ├── pattern.rs      # Pattern struct (v2 schema)
│       ├── event.rs        # ConversationEvent (for Commander)
│       ├── llm.rs          # LLMClient trait + providers
│       └── config.rs       # shared config structs
├── mur-core/               # CLI + library
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs          # public API (for Commander to use)
│       ├── main.rs         # clap CLI entrypoint
│       ├── capture/        # Layer 1: learning pipeline
│       │   ├── mod.rs
│       │   ├── noise_filter.rs
│       │   ├── significance.rs
│       │   ├── extractor.rs
│       │   ├── dedup.rs
│       │   └── linker.rs
│       ├── store/          # Layer 2: storage engine
│       │   ├── mod.rs
│       │   ├── yaml.rs     # YAML read/write
│       │   ├── lancedb.rs  # vector + BM25 index
│       │   └── graph.rs    # pattern link graph
│       ├── retrieve/       # Layer 3: retrieval pipeline
│       │   ├── mod.rs
│       │   ├── gate.rs     # adaptive query gate
│       │   ├── candidate.rs # hybrid search
│       │   ├── scoring.rs  # multi-signal scoring
│       │   ├── postprocess.rs # MMR, budget, tier priority
│       │   └── format.rs   # compress & format for injection
│       ├── evolve/         # Layer 4: evolution engine
│       │   ├── mod.rs
│       │   ├── feedback.rs # collect injection results
│       │   ├── lifecycle.rs # promote/deprecate/archive
│       │   ├── adjuster.rs # Bayesian importance update
│       │   └── linker.rs   # A-Mem style link evolution
│       ├── inject/         # Layer 5: injection runtime
│       │   ├── mod.rs
│       │   ├── hook.rs     # smart hook (Claude Code, Gemini, Auggie)
│       │   └── sync.rs     # smart sync (Cursor, Windsurf, Codex)
│       ├── community/      # Layer 6: sharing
│       │   ├── mod.rs
│       │   ├── publish.rs
│       │   └── fetch.rs
│       └── migrate/        # v1 Go → v2 Rust migration
│           └── mod.rs
└── mur-commander/          # (separate product, future)
    └── ...
```

### 4.2 Public Library API (for Commander integration)

```rust
// mur-core/src/lib.rs
pub async fn capture(transcript: &str, config: &Config) -> Result<Vec<Pattern>>;
pub async fn retrieve(query: &str, context: &RetrieveContext) -> Result<Vec<ScoredPattern>>;
pub async fn evolve(feedback: &Feedback) -> Result<EvolutionResult>;
pub async fn inject(query: &str, context: &InjectContext) -> Result<String>;

// Event bus (for Commander subscription)
pub fn subscribe(event_type: EventType) -> Receiver<MurEvent>;

pub enum MurEvent {
    PatternCreated(Pattern),
    PatternEvolved { id: String, old_importance: f64, new_importance: f64 },
    PatternDeprecated(String),
    InjectionCompleted { patterns: Vec<String>, session_id: String },
}
```

---

## 5. Pattern Schema v2

Backward-compatible with v1 (new fields are optional during migration).

```yaml
# v2 pattern format
schema: 2
name: swift-testing-macro
description: Prefer Swift Testing over XCTest

content:
  technical: |
    Use @Test macro instead of func test...()
    Use #expect() instead of XCTAssert
    Use @Suite for test organization
  principle: |
    When writing new tests, always check if Swift Testing
    is available. Prefer declarative over imperative.

tier: core                    # core | project | session
importance: 0.85              # 0.0-1.0, initial by LLM, adjusted by feedback
confidence: 0.9               # extraction confidence

tags:
  languages: [swift]
  topics: [testing]

applies:
  projects: [bitl]            # or ["*"] for universal
  languages: [swift]
  tools: [claude-code]        # inject only when using this tool
  auto_scope: true            # auto-detect from pwd/git remote

evidence:
  source_sessions: ["2026-02-20-abc123"]
  first_seen: "2026-02-01"
  last_validated: "2026-02-20"
  injection_count: 47
  success_signals: 38
  override_signals: 2
  effectiveness: 0.83         # success / (success + override)

links:
  related: ["swift-result-builder", "swift-concurrency-testing"]
  supersedes: ["xctest-assertions"]
  workflows: []               # MUR Commander workflow refs (future)

lifecycle:
  status: active              # active | deprecated | archived
  decay_half_life: 90         # days (session=14, project=90, core=365)
  last_injected: "2026-02-24"

created_at: "2026-02-01T10:00:00+08:00"
updated_at: "2026-02-24T15:00:00+08:00"
```

### Content length constraint
- Max 500 characters per layer (technical/principle)
- Capture pipeline auto-compresses via LLM if exceeded
- Inspired by LanceDB Pro Plugin Rule 7

---

## 6. Layer Details

### Layer 1: CAPTURE Pipeline

```
Session Transcript
       │
       ▼
┌──────────────┐
│ Noise Filter │  regex-based: greetings, denials, boilerplate, <6 CJK chars
└──────┬───────┘
       ▼
┌──────────────┐
│ Significance │  LLM classification: convention / fix / discovery / preference
│ Detector     │  Skip if score < 0.5
└──────┬───────┘
       ▼
┌──────────────┐
│ Pattern      │  LLM extraction → structured YAML
│ Extractor    │  Dual-layer: technical + principle content
│              │  Max 500 chars per layer
└──────┬───────┘
       ▼
┌──────────────┐
│ Dedup &      │  Semantic dedup: cosine > 0.85 = duplicate
│ Merge        │  Duplicate → merge evidence, don't create new
└──────┬───────┘
       ▼
┌──────────────┐
│ Verify       │  Store → immediately retrieve → if score < 0.5,
│              │  rewrite content and re-embed (LanceDB Pro Rule 6)
└──────┬───────┘
       ▼
┌──────────────┐
│ Link         │  A-Mem: find related patterns, create links,
│ Discovery    │  potentially update old pattern content
└──────────────┘
```

### Layer 2: STORE Engine

```
~/.mur/
├── patterns/            # YAML source of truth (human-editable)
│   ├── swift-testing.yaml
│   └── ...
├── index/               # auto-generated, rebuildable
│   ├── vectors.lance    # LanceDB: vector embeddings + BM25 FTS
│   ├── graph.json       # pattern link adjacency list
│   └── metrics.json     # injection/effectiveness tracking
├── sessions/            # session transcripts
├── commander/           # MUR Commander data (shared storage)
├── config.yaml          # global config
└── state.json           # runtime state
```

**Key decisions:**
- YAML = source of truth. LanceDB = index (rebuildable from YAML anytime)
- `fsnotify` equivalent in Rust (`notify` crate): YAML change → auto rebuild index
- File-level locking (`flock`) for multi-agent concurrent access

**LanceDB vs alternatives:**
- Plan A: `lancedb` Rust crate (native, zero FFI)
- Plan B: `sqlite-vec` if LanceDB Rust API is immature (½ day spike to decide)
- Plan C: `tantivy` BM25 only, defer vector search

### Layer 3: RETRIEVE Engine (5-stage pipeline)

```
User Query (from hook)
       │
       ▼
┌──────────────────┐
│ 1. Adaptive Gate │  Skip: greetings, commands, emoji, <6 CJK chars
│    CJK-aware     │  Force: "remember", "上次", "之前", error keywords
└──────┬───────────┘
       ▼
┌──────────────────┐
│ 2. Candidate     │  Hybrid: vector cosine + BM25 + tag match
│    Selection     │  Auto-scope: pwd/git remote → project filter
│    (LanceDB)     │  Candidate pool: top 20
└──────┬───────────┘
       ▼
┌──────────────────┐
│ 3. Multi-Signal  │  Scoring formula:
│    Scoring       │
│                  │  final = relevance × 0.45
│                  │        + recency × 0.10
│                  │        + effectiveness × 0.15
│                  │        + importance × 0.15
│                  │        + time_decay × 0.10
│                  │        + length_norm × 0.05
│                  │
│                  │  recency = exp(-days / 14)
│                  │  time_decay = 0.5 + 0.5 × exp(-days / half_life)
│                  │  length_norm = 1 / (1 + 0.5 × log2(len / 500))
│                  │  no-scope penalty: × 0.7
└──────┬───────────┘
       ▼
┌──────────────────┐
│ 4. Post-Process  │  Hard floor: score < 0.35 → drop
│                  │  MMR diversity: cosine > 0.85 → deduplicate
│                  │  Tier priority: core > project > session
│                  │  Token budget: max 5 patterns or 2000 tokens
└──────┬───────────┘
       ▼
┌──────────────────┐
│ 5. Format &      │  Inject content only (no metadata)
│    Compress      │  Merge related patterns into single block
│                  │  Choose technical vs principle layer by query type
└──────────────────┘
```

### Layer 4: EVOLVE Engine

**Feedback collection:**
- Hook reports which patterns were injected
- Session end reports success/override signals
- `mur feedback helpful/unhelpful <pattern>` manual feedback

**Importance adjustment (Bayesian):**
```
prior = current importance
likelihood = success_rate from recent 10 injections
posterior = (prior × likelihood) / evidence_weight
new_importance = clamp(posterior, 0.1, 1.0)
```

**Tier auto-promotion:**
```
session → project:
  injection_count >= 5 AND effectiveness >= 0.7

project → core:
  applies.projects.length >= 3 AND effectiveness >= 0.8
  (requires human confirmation via `mur promote <pattern>`)
```

**Lifecycle rules:**
| Status | Condition |
|--------|-----------|
| `active` | Default |
| `deprecated` | 90 days no injection OR effectiveness < 0.3 |
| `archived` | deprecated + 180 days. Moved to `patterns/archive/` |

**Link evolution (A-Mem inspired):**
- New pattern triggers search for related existing patterns
- If found: create bidirectional `related` link
- If new pattern contradicts old: create `supersedes` link, deprecate old

### Layer 5: INJECT Runtime

**Mode A: Smart Hook (Claude Code, Gemini, Auggie)**
```
Hook trigger → mur inject --query="user prompt" --project=$(pwd)
            → Retrieve Engine 5-stage pipeline
            → Output top 3-5 patterns
            → Inject into prompt context
            → Report injected pattern IDs
            → Session end → report success/override
```

**Hook triggers (v2 expansion):**
```rust
enum HookTrigger {
    SessionStart,    // existing behavior
    OnError,         // detect error/failure → recall relevant patterns
    OnRetry,         // detect repeated attempt → inject "how we fixed this before"
    Manual,          // mur inject --query "..."
}
```

**Mode B: Smart Sync (Cursor, Windsurf, Codex, Aider)**
```
mur sync --smart
  → per project: detect git remote → scope patterns
  → rank by multi-signal score
  → write top 20 to .cursorrules / AGENTS.md / .aider.conf
  → recalculate on each sync (not full dump)
```

### Layer 6: COMMUNITY

**Short-term:** improve existing `mur community`
```
mur community publish <pattern>
  → sanitize: strip API keys, internal URLs, file paths
  → attach anonymous evidence data
  → server ranks by cross-user effectiveness

mur community fetch <pattern>
  → download as session tier
  → auto-report effectiveness after use
```

**Long-term:** EvoMap GEP compatibility
```
Pattern ↔ Gene mapping:
  pattern.yaml → GEP Gene + Capsule
  name → gene.id
  content → gene.strategy
  evidence → capsule.signals + capsule.confidence
  tags → capsule.environment_fingerprint
```

---

## 7. Migration (Go v1 → Rust v2)

```bash
mur migrate v2
```

**Steps:**
1. Detect v1 patterns in `~/.mur/patterns/` (schema: 1 or no schema field)
2. Convert to v2 schema:
   - `content` string → `content.technical` (principle left empty)
   - Add `tier: session` (default)
   - Add `importance: 0.5` (default)
   - Add empty `evidence`, `links`, `lifecycle` blocks
3. Run semantic dedup (cosine > 0.85) → estimate 207 → ~120 patterns
4. Build LanceDB index from all patterns
5. Mark unresolvable patterns as `needs_review: true`
6. Print summary: migrated / deduped / needs_review counts

**Config compatibility:** `~/.mur/config.yaml` format preserved. New fields added with defaults.

---

## 8. Embedding Model

```yaml
# ~/.mur/config.yaml
embedding:
  provider: ollama            # ollama | openai
  model: nomic-embed-text     # 768d, free, local
  # Alternative:
  # provider: openai
  # model: text-embedding-3-small
  dimensions: 768
```

- Default: `nomic-embed-text` via Ollama (local, free)
- Optional: OpenAI `text-embedding-3-small` (better quality, needs API key)
- Dimension fixed after first index build. Changing requires `mur reindex`.

---

## 9. User Escape Hatches

```bash
mur pin <pattern>       # never deprecated by lifecycle manager
mur mute <pattern>      # temporarily skip injection, don't delete
mur boost <pattern>     # manually increase importance
mur gc                  # interactive cleanup of low-quality patterns
mur gc --auto           # auto-archive: effectiveness < 0.2 AND age > 60 days
mur reindex             # rebuild LanceDB from YAML (after manual edits)
```

Human control > algorithm. Always.

---

## 10. CLI Commands (v2)

### Preserved from v1 (same interface)
```
mur new                 # create pattern interactively
mur search <query>      # search patterns (now semantic!)
mur learn extract       # extract from session (now with quality gates)
mur sync                # sync to tools (now smart per-project)
mur stats               # show statistics (now with effectiveness)
mur community           # publish/fetch
```

### New in v2
```
mur inject              # smart retrieval for hook integration
mur feedback            # report pattern effectiveness
mur migrate             # v1 → v2 migration
mur gc                  # garbage collection
mur pin/mute/boost      # manual overrides
mur reindex             # rebuild index from YAML
mur links <pattern>     # show pattern connections
mur dashboard           # terminal dashboard (bubbletea equivalent)
mur promote <pattern>   # manually promote tier
mur deprecate <pattern> # manually deprecate
```

---

## 11. Crate Dependencies

```toml
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
clap = { version = "4", features = ["derive"] }
reqwest = { version = "0.12", features = ["json"] }
tracing = "0.1"
tracing-subscriber = "0.3"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
anyhow = "1"
thiserror = "2"

# Storage & retrieval
lancedb = "0.x"               # native Rust — verify version in spike
tantivy = "0.22"              # BM25 fallback / complement
notify = "7"                  # file watching for auto-reindex

# Embedding (local)
# Option A: call Ollama HTTP API via reqwest
# Option B: ort (ONNX Runtime) for fully offline embedding

# CLI UX
ratatui = "0.29"              # terminal dashboard
indicatif = "0.17"            # progress bars
dialoguer = "0.11"            # interactive prompts
console = "0.15"              # colored output
```

---

## 12. Testing Strategy

| Module | Type | Coverage target |
|--------|------|-----------------|
| `capture/noise_filter` | Unit | 100% — pure functions, regex-based |
| `capture/dedup` | Unit | 100% — cosine similarity threshold |
| `retrieve/scoring` | Unit | 100% — **most critical**, one bug ruins all injection |
| `retrieve/gate` | Unit | 95% — CJK-aware query classification |
| `evolve/adjuster` | Unit | 100% — Bayesian math must be correct |
| `evolve/lifecycle` | Unit | 95% — promotion/deprecation rules |
| `store/lancedb` | Integration | 80% — temp DB, insert/search/delete |
| `store/yaml` | Integration | 90% — read/write/migrate real files |
| `inject/hook` | Integration | 70% — mock subprocess |
| `migrate` | Integration | 90% — real v1 patterns → v2 |
| E2E | E2E | Key flows: learn → store → retrieve → inject → feedback |

---

## 13. Cross-compilation & Distribution

| Platform | Method | Priority |
|----------|--------|----------|
| macOS arm64 | Local build | MVP |
| macOS x86_64 | Cross-compile or CI | MVP |
| Linux x86_64 | GitHub Actions CI | MVP |
| Linux arm64 | GitHub Actions CI | P1 |
| Windows | Not supported (WSL only) | — |

```bash
# Install methods
cargo install mur-core          # from crates.io
brew install mur-run/tap/mur    # Homebrew (update Formula)
```

---

## 14. Implementation Roadmap

### Phase 1: Foundation (Week 1-4) — Highest ROI

**Goal:** Working Rust CLI that replaces Go version with quality gates.

| Week | Task | LOC | Deliverable |
|------|------|-----|-------------|
| 1 | Cargo workspace scaffold + `mur-common` types | ~500 | Compiles, shared Pattern struct |
| 1 | CLI framework (clap) + `mur new/search/stats` | ~600 | Basic commands work |
| 2 | YAML store (read/write patterns, v2 schema) | ~400 | `mur new` creates v2 patterns |
| 2 | Migration tool (v1 → v2) | ~300 | `mur migrate v2` works on real data |
| 3 | Capture pipeline: noise filter + significance + extractor | ~500 | `mur learn extract` with quality gates |
| 3 | Capture pipeline: dedup + verify | ~300 | No more junk patterns |
| 4 | Injection tracking (hook reports injected patterns) | ~400 | Closed feedback loop starts |
| 4 | `mur gc` command | ~200 | Clean 207 → ~80 patterns |

**Phase 1 spike (Day 1):** ½ day LanceDB Rust API evaluation. If immature → use tantivy BM25 for Phase 1, defer vectors to Phase 2.

**Phase 1 result:**
- ✅ Rust binary replaces Go binary
- ✅ v2 pattern schema with quality gates
- ✅ Injection tracking (feedback loop begins)
- ✅ 207 → ~80 quality patterns

### Phase 2: Smart Retrieval (Week 5-7)

**Goal:** Semantic search + multi-signal scoring. The biggest user-facing improvement.

| Week | Task | LOC | Deliverable |
|------|------|-----|-------------|
| 5 | LanceDB integration (vector index + BM25 hybrid) | ~800 | Semantic search works |
| 5 | Embedding pipeline (Ollama/OpenAI) | ~300 | Patterns have embeddings |
| 6 | Multi-signal scoring pipeline | ~500 | Precision injection |
| 6 | Adaptive gate + auto-scope (pwd/git) | ~400 | Smart query filtering |
| 7 | Token budget + MMR diversity + post-processing | ~300 | Controlled output |
| 7 | `mur stats` v2 + `mur dashboard` | ~400 | Effectiveness visible |

**Phase 2 result:**
- ✅ Full-dump → precision top-5 injection
- ✅ Tag match → semantic + keyword hybrid search
- ✅ 0% effectiveness tracking → complete feedback loop

### Phase 3: Evolution (Week 8-10)

**Goal:** Patterns learn and evolve automatically.

| Week | Task | LOC | Deliverable |
|------|------|-----|-------------|
| 8 | Pattern tier system (session/project/core) | ~600 | Auto-promotion/demotion |
| 8 | Importance auto-adjustment (Bayesian) | ~400 | Self-tuning |
| 9 | Pattern linking (Zettelkasten) | ~500 | Related patterns connected |
| 9 | Smart sync (per-project top-20) | ~400 | Cursor/Windsurf upgrade |
| 10 | Lifecycle manager (deprecate/archive) | ~300 | Auto-cleanup |
| 10 | OnError/OnRetry hook triggers | ~300 | Recall-before-retry |

### Phase 4: Community & GEP (Week 11-14)

**Goal:** Social pattern evolution.

| Week | Task | LOC |
|------|------|-----|
| 11-12 | Community effectiveness reporting + sanitization | ~500 |
| 12-13 | GEP protocol adapter | ~800 |
| 13-14 | Team shared patterns with evidence | ~600 |

---

## 15. MUR Commander Integration Points

Commander (resident daemon) integrates with Core via Cargo workspace:

```
Commander → use mur_core::retrieve::search()   # library call, zero overhead
Commander → mur_core::subscribe(EventType::*)   # event bus subscription
Commander → shared ~/.mur/index/vectors.lance   # same LanceDB instance
```

**Specific integration scenarios:**

1. **Workflow extraction enrichment:** Commander extracts workflow → calls `mur_core::retrieve()` → finds related atomic patterns → links workflow steps to patterns
2. **Session feedback:** Commander's Completion Detector confirms task success → calls `mur_core::evolve()` → patterns used in that session get effectiveness boost
3. **Shared storage:** Both products read/write `~/.mur/` with file-level locking

**Dependency:** Core v2 Phase 1-2 must complete before Commander Phase 1 begins (~6-8 week lead).

---

## 16. MUR Server Impact

Minimal changes needed:

| Component | Change | Effort |
|-----------|--------|--------|
| REST API | Add optional v2 fields (evidence, tier, links) | 1-2 days |
| Pattern storage | Accept schema: 2 patterns | 1 day |
| Ranking | Use cross-user effectiveness data | 2-3 days |
| Frontend (mur.run) | Display effectiveness metrics | 2-3 days |

API contract is backward-compatible. v1 clients still work.

---

## 17. Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| LanceDB Rust SDK immature | High | Phase 1 spike; fallback to sqlite-vec or tantivy-only |
| Embedding quality variance | Medium | Default nomic-embed-text; allow swap + `mur reindex` |
| Migration loses data | High | `mur migrate` is non-destructive; copies, doesn't move |
| Scope creep | High | Phase 1 = replace Go 1:1 + quality gates. No new UX until Phase 2 |
| Single developer bottleneck | Medium | AI-assisted development; modular crates enable parallel work |

---

## 18. Success Metrics

| Metric | v1 (current) | v2 Phase 1 | v2 Phase 3 |
|--------|-------------|------------|------------|
| Pattern count | 207 (unfiltered) | ~80 (quality-gated) | ~60 (evolved) |
| Injection tracking | 0% | 100% | 100% |
| Avg effectiveness | 50% (fake) | measured | >70% |
| Retrieval method | tag match | semantic hybrid | semantic + temporal |
| Pattern lifecycle | none | manual gc | auto promote/deprecate |
| Token waste per injection | ~5000 (full dump) | ~2000 (budget) | ~1500 (compressed) |

---

## 19. Architecture Decision Records

### ADR-1: Why Rust over Go?
LanceDB is Rust-native. Commander is Rust. >60% of Go code needs rewriting anyway for v2 features. Single language = single binary = shared crates.

### ADR-2: Why YAML stays as source of truth?
Mur's soul. Human-readable, vim-editable, git-friendly. Differentiator vs Mem0/Zep black boxes. LanceDB index is always rebuildable from YAML.

### ADR-3: Why 3 tiers (not 2 or 5)?
2 is too coarse (no transition between session and permanent). 5 is too complex. 3 maps to MemoryOS STM/MTM/LPM with academic backing.

### ADR-4: Why not MCP server?
Mur's value is cross-tool. MCP serves single client. Hook mechanism already covers Claude Code + Gemini + Auggie. Can add MCP interface as supplement later.

### ADR-5: Why dual-layer content (technical + principle)?
Inspired by LanceDB Pro Plugin Rule 6. Different query types need different content. "How to do X" → technical. "Should I do X" → principle.

### ADR-6: Why 500 char limit per content layer?
LanceDB Pro Plugin Rule 7. Short, atomic patterns retrieve better and waste fewer tokens. LLM auto-compresses if exceeded.

---

## 20. One-Line Summary

**Mur v1 is a notebook. Mur v2 is a brain.** Notebooks don't forget and don't learn. Brains strengthen useful memories, fade useless ones, build connections, and know what to recall when. YAML is the skeleton (stable, human-controlled). Retrieve + Evolve engines are the nervous system (dynamic, adaptive, closed-loop). The skeleton stays. The nervous system gets built from scratch — in Rust.

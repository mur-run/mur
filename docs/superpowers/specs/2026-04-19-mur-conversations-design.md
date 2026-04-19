# mur Conversations Archive — Design Spec

**Date:** 2026-04-19
**Status:** Draft, pending implementation plan
**Prior art:** `docs/superpowers/specs/2026-04-18-mur-commander-memory-sync-design.md`

---

## 1. Purpose & Problem Statement

mur needs a local-only, queryable archive of conversations from every AI coding assistant and chat platform the user interacts with — so the user can browse by date, search by keyword, and ask natural-language questions ("what compression techniques did I discuss last week?") across their complete interaction history.

**Concrete motivation.** Today:
- Conversations are captured only when `mur in` starts a session; most everyday use is not recorded.
- Commander already writes 3 parallel memory stores under `~/.mur/commander/` with overlapping but incompatible schemas.
- Claude Code hook calls `mur_learn::session::record()` from commander gateway, producing triple-write duplication with the planned new location.
- No unified retrieval surface exists; patterns are accessible via `mur search`, but the underlying conversations are not.

**Goal.** One canonical archive at `~/.mur/conversations/` that both mur and commander write to and read from, supports the three query modes, and compresses aggressively without losing Q&A fidelity.

**Non-goals.**
- Cross-device cloud sync (this archive is local-only by design).
- ChatGPT / Claude.ai web-UI capture (requires browser extension; deferred).
- Meeting transcription integration (Circleback already handles it; deferred).
- Pattern extraction pipeline changes (patterns/ and session/recordings/ stay intact).

---

## 2. Scope & Decisions Made

### In scope (MVP)

| Dimension | Decision |
|---|---|
| Sources | Claude Code · Cursor · Gemini CLI · Aider · Slack · Telegram · Discord · Commander engine |
| Storage | Local-only at `~/.mur/conversations/` |
| LLM | Local Ollama (qwen3:14b for generation, qwen3-embedding:0.6b for vectors) |
| Retention | Raw JSONL kept `retention_days` (default 30); daily summaries kept forever |
| Integration strategy | **Option X — Rename & Unify** — migrate commander's existing stores into the new shared directory |
| Query modes | Timeline browse · Semantic+keyword search · NL Q&A with citation |
| CLI verbs (design, phased) | Phase 1: `mur chat list` · `mur chat show` · `mur chat raw` · `mur chat search` · `mur conversations {pull,compact,reindex,doctor,migrate,rollback}` · Phase 2: `mur ask` |

### Out of scope (MVP)

- Codex CLI ingester (Phase 3)
- LLMLingua-2 pre-summarization (Phase 3)
- RAPTOR week/month roll-ups (Phase 3)
- A-MEM dynamic re-linking on insert (Phase 3)
- mur-web dashboard implementation (Phase 2)
- Mode C (Ask) — Phase 2 (Phase 1 ships Mode A+B)

### Explicitly rejected alternatives

- **Option Y — Hub-and-Spoke**: commander unchanged, mur tail-reads. Rejected: data duplication, tail lag, schema drift risk.
- **Option Z — Stop mur_learn double-write only**: partial solution, still leaves two directory structures coexisting.
- **TurboQuant** (arXiv 2504.19874) for vector index: no Rust implementation; LanceDB's built-in RaBitQ gives 90% of the benefit.
- **LLM-as-bytestream-codec**: Ollama is not bit-exact across versions; cannot decode what was encoded six months earlier.

---

## 3. Architecture

### 3.1 Final disk layout

```
~/.mur/
├── patterns/*.yaml                  (unchanged)
├── session/recordings/*.jsonl       (unchanged — pattern extraction input)
├── workflows/*.yaml                 (unchanged)
│
├── conversations/                   NEW — unified archive
│   ├── raw/<YYYY-MM-DD>/
│   │   ├── cc_<session>.jsonl
│   │   ├── cursor_<chat>.jsonl
│   │   ├── gemini_<session>.jsonl
│   │   ├── aider_<chat>.jsonl
│   │   ├── slack_<channel>.jsonl
│   │   ├── telegram_<chat>.jsonl
│   │   ├── discord_<channel>.jsonl
│   │   └── commander_<workflow>.jsonl
│   ├── users/<user_id>/
│   │   └── conversation.jsonl       (per-user, layout inherited from commander)
│   ├── summary/
│   │   ├── <YYYY-MM-DD>.md          (hybrid extractive+abstractive)
│   │   └── <YYYY-MM-DD>.yaml        (frontmatter + provenance)
│   ├── index.lance/                 (LanceDB with RaBitQ compression)
│   └── audit.jsonl                  (hash-chained, inherits commander's chain)
│
└── commander/                       (config/logs remain; memory moves)
    ├── config.toml
    ├── logs/
    ├── outbox/                      (signals → mur)
    └── (memory/ directory migrated to conversations/)
```

### 3.2 Module layout

**mur-core/src/conversations/**
```
mod.rs              public API entry
schema.rs           Message, Role, Content, Source types
store.rs            raw JSONL read/write (trait impl'd by commander too)
index.rs            LanceDB + RaBitQ wrapper
summarize.rs        hybrid summary + Freedman macro-expansion
retrieve.rs         timeline/search/ask implementations
retention.rs        30d cleanup with three-guard safety
migrate.rs          commander → conversations migrator
ingest/
├── mod.rs          trait Ingester
├── claude_code.rs  routes from session/recordings/
├── cursor.rs       reads SQLite state.vscdb + .specstory/
├── gemini.rs       reads ~/.gemini/tmp/<hash>/chats/*.json
├── aider.rs        scans configured watched_dirs for .aider.chat.history.md
├── normalize.rs    tool-call pointer substitution
├── dedup.rs        MinHash near-duplicate detection
└── filter.rs       Mem0-style REJECT gate
```

**mur-common/src/conversation.rs (new)**
```rust
pub struct Message {
    pub ts: DateTime<Utc>,
    pub source: Source,
    pub conv_id: String,
    pub role: Role,
    pub content: Content,
    pub metadata: serde_json::Value,
    pub pattern_refs: Vec<String>,
}

pub enum Source { ClaudeCode, Cursor, Gemini, Aider, Slack, Telegram, Discord, CommanderEngine }
pub enum Role { User, Assistant, System, Tool }
pub enum Content {
    Text(String),
    ToolRef { sha256: String, path: String, bytes: u64, desc: String },
    ImageRef { sha256: String, path: String, desc: String },
}
```

**commander side — minimal changes**
- `crates/engine/src/memory/long_term.rs` — path constant changes to `~/.mur/conversations/raw/<today>/commander_engine_<id>.jsonl`
- `crates/gateway/src/memory/episodes.rs` — paths change to `~/.mur/conversations/users/<uid>/` and `~/.mur/conversations/raw/<today>/<platform>_<channel>.jsonl`
- `crates/gateway/src/memory/lance_store.rs` — points at `~/.mur/conversations/index.lance`
- `crates/gateway/src/unified_handler/mod.rs` — removes the 5 `mur_learn::session::record()` call sites (commander is no longer a second writer; mur's ingester owns that)
- Working and short-term memory layers stay unchanged (RAM-only, no disk path).

### 3.3 Data flow

```
   mur ingesters             commander adapters
   (cc/cursor/gemini/        (slack/telegram/discord/
    aider polling or hook)    engine direct write)
         │                            │
         └────────────┬───────────────┘
                      ▼
           ┌────────────────────────┐
           │ Pre-filter pipeline    │  pure Rust, no LLM
           │ 1. normalize (tool ptr)│
           │ 2. dedup (MinHash ≥0.85)│
           │ 3. filter (REJECT gate)│
           └──────────┬─────────────┘
                      ▼
           raw/<date>/<src>_<id>.jsonl
                      │
                      ├──► index.lance (synchronous upsert, RaBitQ)
                      ▼
           ┌────────────────────────┐
           │ Sleep-time compact job │  daemon-triggered or cron
           │ - summarize hybrid     │
           │ - macro-expand refs    │
           │ - link to patterns     │
           └──────────┬─────────────┘
                      ▼
           summary/<date>.{md,yaml}
                      │
           30 days later ◄──────────
                      ▼
           raw/<date>/ deleted after
           three-guard safety check
           (summary exists + provenance
           verified + audit recorded)
```

---

## 4. Storage Format

### 4.1 Raw JSONL row

```json
{"v":1,
 "ts":"2026-04-19T11:30:45Z",
 "src":"claude-code",
 "conv":"3a8786a0",
 "role":"user",
 "content":{"t":"text","v":"修一下 yaml.rs 的 atomic write bug"},
 "meta":{"project":"mur","cwd":"/V/mur"},
 "refs":["pattern:atomic-yaml-write"]}
```

Rules:
- `v: 1` is the schema version; breaking changes bump this.
- `content.t` is one of `"text"`, `"tool_ref"`, `"image_ref"`.
- Any single row that exceeds 10 KB must use `tool_ref` or `image_ref` (the Section 4.3 pointer substitution).
- `refs` is a list of `"pattern:<name>"` strings — Named-Abstraction references into `patterns/`, applied at summarization and expanded at retrieval.

### 4.2 Daily summary (hybrid)

```markdown
---
date: 2026-04-19
conv_count: 12
msg_count: 487
sources: [claude-code, cursor, slack]
pattern_refs: [atomic-yaml-write, mur-sync-phase-1, lancedb-rabitq]
keywords: [conversations, freedman, compression, ingest]
links: [./2026-04-18.md, ./2026-04-20.md]
---

## Extractive spans

- _{claude-code/3a8786a0 @offset 1234-1456}_:
  > 修一下 yaml.rs 的 atomic write bug
- _{slack/C0ARLGHP3A5 @ts 11:31:02}_:
  > 把 commander 記憶系統整合到 conversations/

## Abstractive narrative

今天主要有兩條線:(1) brainstorm mur 對話記憶架構,採用 {{pattern: atomic-yaml-write}}
作為寫入模型;(2) 研究壓縮理論 ({{pattern: freedman-compression}}) 並決定方案 X
整合 commander。

## Macro expansion map

{{pattern: atomic-yaml-write}} → patterns/atomic-yaml-write.yaml
{{pattern: freedman-compression}} → patterns/freedman-compression.yaml
```

Q&A retrieval reads extractive spans first (zero hallucination, grounded in exact offsets), falls back to abstractive prose only when nothing matches.

### 4.3 Tool-call pointer substitution

Claude Code / Cursor / Aider conversations are 60–80% tool-result bytes. Before anything touches Ollama, replace tool-result bodies with pointer records:

Before:
```json
{"role":"tool","content":{"t":"text","v":"<14 KB of `cargo build` output>"}}
```

After:
```json
{"role":"tool","content":{"t":"tool_ref","sha256":"a3f1...","path":"<cache>/a3f1...","bytes":14820,"desc":"cargo build (warnings x3)"}}
```

Content is hashed and written to a content-addressed store under `~/.mur/conversations/blob/<sha256>` only when the original content isn't already persisted elsewhere (e.g., a file read with a still-existing path skips blob storage and uses the live file path).

### 4.4 LanceDB schema

| Column | Type | Purpose |
|---|---|---|
| `id` | Utf8 | `<src>_<conv>_<line>` composite primary key |
| `ts` | Int64 | Unix seconds for range queries |
| `source` | Utf8 | Filter field |
| `conv_id` | Utf8 | Locate origin raw file |
| `role` | Utf8 | User / Assistant / System / Tool |
| `layer` | Int8 | 0=message, 1=day-summary, 2=week-summary (layer 1/2 reserved for Phase 3 RAPTOR) |
| `content` | Utf8 | Text content or tool_ref description |
| `vector` | FixedSizeList\<Float32, 1024\> | qwen3-embedding output, RaBitQ-compressed |

Index is always rebuildable from raw JSONL via `mur conversations reindex`.

### 4.5 Audit hash chain

```rust
pub struct AuditEntry {
    pub id: Uuid,
    pub ts: DateTime<Utc>,
    pub action: AuditAction,
    pub target_path: String,
    pub sha256_content: String,
    pub prev_hash: String,
    pub entry_hash: String,  // sha256(prev_hash || canonical_json(action, target, content_hash))
}

pub enum AuditAction { Write, Summarize, Index, Delete, Migrate, Rollback, Error }
```

Chain is initialized from the last `entry_hash` in commander's existing `audit.jsonl`. Migration writes a single `Migrate` entry linking old and new chains.

### 4.6 Retention algorithm

```rust
// conversations/retention.rs
pub async fn cleanup(now: DateTime<Utc>, retention_days: u32) -> Result<Report> {
    for date_dir in list_raw_dirs()? {
        if (now - date_dir.date).num_days() < retention_days as i64 { continue; }

        // Guard 1: must have summary
        let summary = summary_path(date_dir.date);
        if !summary.exists() {
            warn!("skipping {date_dir}: no summary yet");
            continue;
        }

        // Guard 2: provenance must verify
        if !verify_provenance(&summary, &date_dir)? {
            error!("provenance mismatch at {date_dir}; aborting delete");
            continue;
        }

        // Guard 3: audit the deletion
        audit.record(AuditAction::Delete { target: date_dir.clone(),
                                           reason: "retention" })?;
        fs::remove_dir_all(&date_dir)?;
    }
    Ok(report)
}
```

`retention_days` lives in `~/.mur/config.yaml` under `conversations.retention_days` (default `30`).

---

## 5. Ingesters

### 5.1 Common trait

```rust
pub trait Ingester: Send + Sync {
    fn name(&self) -> &'static str;
    fn strategy(&self) -> IngestStrategy;  // RealTime | Poll(Duration) | Manual
    async fn pull(&mut self, cursor: &mut Cursor) -> Result<Vec<Message>>;
    fn normalize(&self, raw: serde_json::Value) -> Result<Message>;
}
```

### 5.2 Real-time ingesters

- **Claude Code** — `mur session record` reroutes to `conversations::write()`; the legacy `session/recordings/` path is kept for pattern extraction (dual-read from the same source is fine; dual-write is what we're eliminating).
- **Commander engine** — `long_term.rs` writes directly to `raw/<today>/commander_engine_<id>.jsonl`.
- **Commander Slack/Telegram/Discord** — gateway adapters write to `raw/<today>/<platform>_<channel>.jsonl`; the `mur_learn::session::record()` call sites are removed.

### 5.3 Polling ingesters (5-minute interval)

- **Cursor** — reads `~/Library/Application Support/Cursor/User/workspaceStorage/*/state.vscdb` via `rusqlite`, querying `ItemTable WHERE key LIKE '%aiChat%'`. Also reads `.specstory/` if present. Cursor schema is unstable; parser is best-effort with skip-on-failure.
- **Gemini CLI** — reads `~/.gemini/tmp/<hash>/chats/*.json`. Tracks per-file mtime cursor.
- **Aider** — scans dirs listed in `config.yaml` `conversations.aider.watched_dirs` for `.aider.chat.history.md`. Markdown is split on `####` and `---`.

### 5.4 Pre-filter pipeline (all ingesters)

| Stage | Module | Purpose | Failure mode |
|---|---|---|---|
| 1 | `normalize.rs` | Tool-call pointer substitution; 60–80% size reduction | Keep original content |
| 2 | `dedup.rs` | MinHash near-dup at threshold 0.85 | Pass through |
| 3 | `filter.rs` | REJECT gate (Mem0 lesson): heartbeat, system-prompt restatement, empty turn | Pass through |

Failure in any stage degrades to "write raw anyway, skip that optimization" — never blocks ingestion.

---

## 6. Retrieval

### 6.1 CLI — three-tier progressive disclosure

| Tier | Command | Approx. tokens |
|---|---|---|
| 1 Index | `mur chat list [--since] [--until] [--src]` | ~200 |
| 2 Summary | `mur chat show <date> [--expand=0\|1\|full]` | ~2,000 |
| 3 Raw | `mur chat raw <date> <conv_id>[:offset]` | ~20,000 |

Additional CLI:
- `mur chat search <query>` — Mode B (semantic + keyword)
- `mur ask <question>` — Mode C (RAG with citation)
- `mur conversations pull` — force polling cycle
- `mur conversations compact` — force summarization of pending days
- `mur conversations reindex` — rebuild LanceDB from raw
- `mur conversations doctor` — health check
- `mur conversations migrate --dry-run|--run`
- `mur conversations rollback --to-commander`

### 6.2 Filter flags (all modes)

```
--since <date> / --until <date>
--src <list>   (comma-separated allowlist)
--src !<list>  (denylist)
--role user|assistant|tool|system
--project <name>
--user <user_id>
--min-score <float>  (search/ask only)
```

### 6.3 Mode A — Timeline browse

Pure file read. `summary/<date>.md` takes precedence; if missing, assemble from raw and emit on the fly, cache as summary. No vector search.

### 6.4 Mode B — Search

```
query → embed (qwen3-embedding:0.6b)
      → LanceDB search (layer=0, k=30, RaBitQ)
      → MMR diversification (threshold 0.85, same as patterns)
      → BM25 keyword rerank (0.7 vector + 0.3 keyword, consistent with existing mur scoring)
      → top 5 snippets with source pointers
```

### 6.5 Mode C — Ask (RAG with tiered escalation)

```rust
async fn ask(query: &str) -> Answer {
    let hits = index.search(query, layer=1, k=5);      // summary first
    let hits = if hits.top_score() < 0.5 {
        index.search(query, layer=0, k=10)             // escalate to raw
    } else { hits };
    let spans = hits.iter().flat_map(|h| h.extractive_spans()).collect();
    let expanded = macro_expand(spans);                // Freedman
    let context = render_context(query, expanded);     // ~3,500 tokens
    ollama.generate(&context, model="qwen3:14b").await
}
```

Responses include Perplexity-style citations: `[cit: <date> <src>/<conv>:<offset>]`. Never fabricate citations; if nothing matches, answer "no relevant conversations in the last N days."

### 6.6 Failure degradation (Mode C)

| Failure | Behavior |
|---|---|
| Ollama unreachable | Degrade to Mode B; explicitly say "these are snippets, not an answer" |
| No vector index | Degrade to pure BM25 |
| Context window exceeded | Shrink k: 5 → 3 → 1 |
| No matches | Return "no relevant conversations in the last <retention_days> days" — never fabricate |

### 6.7 REST API (for future mur-web dashboard, Phase 2)

```
GET  /api/conversations?since=&until=&src=      → Tier 1 index
GET  /api/conversations/:date                    → Tier 2 summary
GET  /api/conversations/:date/:conv              → Tier 3 raw (streamed JSONL)
POST /api/conversations/search  {q, filters}
POST /api/conversations/ask     {q, filters}     → SSE stream
```

### 6.8 Freedman macro-expansion

Summaries include `{{pattern: name}}` markers. At retrieval:

```rust
fn expand_macros(summary: &Summary, depth: usize) -> Expanded {
    let mut out = summary.abstractive.clone();
    for re in find_macro_refs(&out) {
        let pattern = yaml_store.get(&re.name)?;
        let replacement = if depth > 0 {
            expand_macros(&pattern.into_summary(), depth - 1)
        } else {
            pattern.one_liner()
        };
        out.replace(re.marker, &replacement);
    }
    out
}
```

Flags: `--expand=0` (none), `--expand=1` (one level, default), `--expand=full` (recursive to fixed point). Mode C decides per query during RAG context assembly.

---

## 7. Migration

### 7.1 Commander → conversations

```bash
# 0. Backup (user-initiated)
cp -r ~/.mur/commander/memory ~/.mur/commander/memory.bak

# 1. Dry-run
mur conversations migrate --dry-run
# Reports: N users, M episodes, audit chain valid, free space needed

# 2. Actual migration
mur conversations migrate --run
# Steps:
#  - Stage to ~/.mur/.conversations-migrating/
#  - Read commander/memory/long_term.jsonl → bucket by date → raw/<date>/commander_engine_<id>.jsonl
#  - Read commander/users/*/conversation.jsonl → users/<uid>/conversation.jsonl
#  - Read commander/memory/episodes/*/*.md → summary/<date>.md (upgraded to hybrid)
#  - Read commander/audit.jsonl → conversations/audit.jsonl (chain preserved)
#  - Rebuild LanceDB with RaBitQ compression
#  - atomic rename staging → final
#  - Verify: replay audit chain, confirm entry_hash matches

# 3. Compatibility symlinks (one-version grace period)
ln -s ~/.mur/conversations/users ~/.mur/commander/users
ln -s ~/.mur/conversations/index.lance ~/.mur/commander/memory/memory.lance

# 4. Commander v0.8 ships with new paths; symlinks keep old reads working

# 5. Commander v0.9 removes symlink fallback
```

### 7.2 Migration risks & mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Commander daemon running during migration | Data loss, broken hash chain | Migrator detects running daemon and refuses |
| Power loss mid-migration | Partial raw + index inconsistency | Staging dir + atomic rename; replay audit on restart |
| Audit chain mis-link | Untrusted audit history | Post-migration verification replays chain; rollback on failure |
| User upgrades mur before commander v0.8 | Mur expects new paths, commander writes old | `legacy_fallback: true` lets mur read both for one version |
| Insufficient disk space | Migration aborts mid-way | Pre-check: `free_space > current_usage × 1.5` |
| Pattern refs pointing to moved paths | Broken references in patterns/ | Migration builds path-rewrite table and updates pattern refs |

### 7.3 Rollback

```bash
mur conversations rollback --to-commander
# - Copy conversations/* back to commander/memory/
# - Rewrite patterns/refs in reverse
# - Append Rollback entry to audit.jsonl
# - Commander will read old paths on next start
```

Rollback is supported for at least 2 minor versions after initial migration.

---

## 8. Error Handling & Observability

### 8.1 Degradation ladder

Writes must never block usability. Each layer degrades independently:

| Layer | Failure | Behavior |
|---|---|---|
| Ingester | Source format changed | Warn, skip this source this cycle, retry next |
| Normalize | Hash computation error | Keep original content |
| Dedup | MinHash crash | Pass through |
| Filter | REJECT rule error | Pass through |
| Store (raw write) | Disk full / permission | **Error**; notify outbox; stop |
| Index | LanceDB write fail | Queue for retry; raw write still succeeds |
| Summarize | Ollama unreachable | Defer to next compact cycle; retention won't delete raw without summary |
| Retention | Any guard fails | **Never delete**; log warn |

### 8.2 Audit entries

All mutating operations append to `audit.jsonl`:
- `Write(source, conv_id, bytes)`
- `Summarize(date, model, duration_ms)`
- `Index(date, vectors_added)`
- `Delete(target_path, reason, bytes_freed)`
- `Migrate(from, to, count)`
- `Rollback(from, to, count)`
- `Error(layer, reason, context)`

### 8.3 `mur conversations doctor`

Reports on: summary coverage, index health (vector count, compression ratio), provenance mismatches, Ollama model availability, audit chain integrity.

---

## 9. Testing Strategy

### 9.1 Unit tests (per module)

- `schema.rs` — Message serialization round-trip (including all `Content` variants)
- `ingest/*.rs` — Each ingester: fixture → `normalize()` → assert fields
- `normalize.rs` — tool_ref pointer substitution and reverse resolution
- `dedup.rs` — Identical content deduped; 0.8-similar content preserved
- `filter.rs` — Table-driven REJECT rules (heartbeat / system-restatement / empty)
- `summarize.rs` — Mock Ollama; verify extractive-span offsets land on real text
- `retention.rs` — Three-guard safety: missing-summary skip; provenance-fail skip; audit before delete

### 9.2 Integration tests

- End-to-end pipeline: synthetic Claude Code events → full pipeline → files + index correct
- Migration: fake commander directory → migrate → verify new structure and audit continuity
- Cross-source: same-timestamp events from different sources coexist; index classifies correctly

### 9.3 Golden-path smoke script

`scripts/golden-path-conversations.sh` (pattern borrowed from `scripts/golden-path-1.sh`):

1. Isolated `$HOME` under `mktemp`
2. Seed 3 sources (cc, cursor, slack) with raw events
3. `mur conversations pull` — verify `raw/<date>/` files generated
4. `mur conversations compact` — verify `summary/<date>.md` generated
5. `mur chat list` / `show` / `raw` — verify three tiers render correctly
6. `mur chat search "LanceDB"` — verify vector search hits
7. `mur ask "什麼是 RaBitQ?"` — verify citation points to correct offset
8. Virtual time-jump 30 days + `mur conversations cleanup` — verify raw deleted, summary retained
9. `mur conversations doctor` — verify all checks pass

### 9.4 Property-based tests

- Audit hash chain invariant: `entry_hash_n = sha256(entry_hash_{n-1} || content_n)`
- Idempotency: writing the same event twice does not duplicate (dedup absorbs)
- Retention ordering: only "older than retention_days AND summary exists AND provenance verified" is a deletable state

---

## 10. Phased Rollout

### Phase 1 — Foundation (MVP)
- Sections 3–4 core write/read path (Mode A + B)
- Ingesters: Claude Code, Cursor, Gemini, Aider, commander engine + Slack/TG/Discord adapters
- Migration tool with dry-run
- `golden-path-conversations.sh` green

### Phase 2 — Intelligence
- Mode C (Ask) with citations
- Sleeptime compact job
- Freedman macro-expansion at retrieval
- mur-web `/conversations` dashboard
- Commander v0.8 integration release

### Phase 3 — Optimization
- LLMLingua-2 pre-summarization (requires Python sidecar or ONNX export)
- RAPTOR weekly/monthly roll-ups at `layer=1`, `layer=2`
- A-MEM dynamic re-linking during sleeptime
- Codex CLI ingester

---

## 11. Configuration

New section in `~/.mur/config.yaml`:

```yaml
conversations:
  enabled: false            # off by default; first-run asks y/n
  retention_days: 30        # raw JSONL retention
  migrate_on_start: ask     # ask | auto | skip
  legacy_fallback: true     # read old commander paths during v0.8 grace period
  poll_interval: 300s
  sources:
    claude_code: { enabled: true }
    cursor:      { enabled: true }
    gemini:      { enabled: true }
    aider:
      enabled: true
      watched_dirs: [~/Projects]
  filter:
    dedup_threshold: 0.85
    reject_heartbeat: true
    reject_system_restatement: true
  summarize:
    model: qwen3:14b        # Ollama model for abstractive narrative
    schedule: idle          # idle | cron:<spec> | manual
  ask:
    model: qwen3:14b
    max_context_tokens: 3500
```

Commander reads `conversations.enabled` and `conversations.retention_days` from mur's `config.yaml` via a shared `mur-common::config` helper; commander's own `config.toml` continues to hold commander-specific settings (logs, outbox, etc.).

---

## 12. Compatibility Constraints (Hard)

These are non-negotiable; any implementation choice must honor them:

1. **Schema forward compatibility.** Commander's existing `ConversationTurn { ts, role, text }` must deserialize as a subset of the new `Message` format (extra fields default to empty).
2. **Append-only.** No file rewrites on `raw/*.jsonl`, `summary/*.md`, or `audit.jsonl`. Soft-delete via metadata flag only.
3. **Audit hash chain continuity.** Migration preserves commander's existing chain; never resets.
4. **`schedules.yaml` untouched.** Commander's existing read behavior stays identical.
5. **No triple-write.** `mur_learn::session::record()` is removed from commander gateway; the new `conversations/` is the canonical writer, mur reads from it to feed pattern extraction.

---

## 13. Open Questions

None blocking Phase 1 implementation. Flagged for Phase 2 discussion:

- Should `conversations.enabled` be `true` by default after 2 stable releases?
- Should commander publish its own audit chain segment separately, then merge at migration time — or write directly into the shared chain from the start?
- Phase 3 RAPTOR: monthly summaries summarize-of-summaries or re-read raw? (tradeoff: speed vs fidelity)

---

## 14. References

- Freedman, Aksenov, Bodnia, Mulligan. *Compression is all you need: Modeling Mathematics.* arXiv 2603.20396 (2026-03-20). — theoretical foundation for Named-Abstraction compression (§4.2, §6.8).
- Google Research. *TurboQuant: Redefining AI Efficiency with Extreme Compression.* arXiv 2504.19874 (ICLR 2026). — skipped in favor of LanceDB RaBitQ; revisit once Rust port exists.
- Delétang et al. *Language Modeling Is Compression.* arXiv 2309.10668 (ICLR 2024). — justifies "summary as compression" framing; not used as literal codec.
- Mem0 production audit. GitHub issue #4573. — 97.8% junk finding drove Mem0-style REJECT gate (§5.4).
- Sarthi et al. *RAPTOR.* arXiv 2401.18059 (ICLR 2024). — Phase 3 plan for week/month roll-ups.
- Packer et al. *MemGPT.* arXiv 2310.08560. — tiered retrieval policy (summary-first, escalate-to-raw) adopted in Mode C.
- A-MEM: *Agentic Memory for LLM Agents.* arXiv 2502.12110 (NeurIPS 2025). — progressive disclosure pattern adopted for CLI.
- Letta *Sleep-time Compute.* — design pattern for idle-time compaction (§5.5).
- LLMLingua-2. — Phase 3 candidate for pre-summarization.
- LanceDB RaBitQ quantization. — MVP vector index choice.

Prior mur design:
- `docs/superpowers/specs/2026-04-18-mur-commander-memory-sync-design.md`
- `docs/mur-函數清單與競品分析.md` (2026-04-18 update)

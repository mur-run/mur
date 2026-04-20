# mur Conversations Archive — Phase 2 Design Spec

**Date:** 2026-04-20
**Status:** Draft, pending implementation plan
**Prior art:** `docs/superpowers/specs/2026-04-19-mur-conversations-design.md` (Phase 1 shipped, mur-run/mur#5 + mur-run/mur-commander#9 merged)
**Supersedes:** Phase 1 §6.5 (Mode C algorithm sketch) — replaced by §5 of this document
**Amends:** Phase 1 §12 (append-only) — narrowed per §8.1 of this document

---

## 1. Purpose & Problem Statement

Phase 1 shipped the `~/.mur/conversations/` archive with Mode A (timeline browse) and Mode B (hybrid semantic+keyword search). It captures, stores, and indexes conversations from every supported AI assistant. **What it does not yet do** is summarize days or answer natural-language questions about them.

**Concrete motivation.** With Phase 1 alone:

- Retention's three-guard safety REQUIRES a summary to exist before deleting raw at T+retention_days. Without a compact pipeline, retention is effectively disabled — the user either keeps raw forever or loses data without provenance.
- Mode B returns snippets but no synthesis. "What compression techniques did I discuss this month?" requires the user to skim top-k results and build the answer themselves.
- No feature-complete Mode C as sketched in Phase 1 §6.5 — that algorithm was pseudocode, not implementation.

**Goal.** Two tightly-coupled deliverables:

1. **Sleep-time compact** — daily hybrid summaries (extractive spans + abstractive narrative + Freedman macro-references) generated automatically and on-demand.
2. **Mode C — `mur ask <question>`** — local-only RAG over the archive with inline Perplexity-style citations, streaming tokens, graceful degradation.

**Non-goals (Phase 2).**
- Multi-turn chat (`--continue` session state). Phase 3.
- mur-web `/conversations` dashboard UI. Phase 3.
- LLMLingua-2 prompt compression. Phase 3.
- RAPTOR week/month summaries (layer=2+). Phase 3.
- Dynamic re-linking when patterns change. Phase 3.

---

## 2. Scope & Decisions Made

### In scope (Phase 2)

| Dimension | Decision |
|---|---|
| Execution model | CLI-first (`mur conversations compact` + `mur ask`) with **commander daemon piggyback** via cron trigger when commander is running |
| Compact scope | Process all **missing completed days** (`date < today_utc`), with throttle (`max_days_per_run = 7` default), plus explicit `--date` / `--since` / `--force` / `--if-stale` overrides |
| Ask output | **Inline `[cit: ...]` citations** + `--json` flag for structured output. Streaming tokens to stdout by default |
| Summary regeneration | **Overwrite allowed** with automatic `.history/` archive of the prior version. §12 narrowed (see §8.1) |
| LLM | Local Ollama only. Default model `qwen3:14b` for both compact and ask |
| Retrieval | Tiered escalation: layer=1 (summary) → layer=0 (raw) when top score < `escalation_threshold` (default 0.5) |
| Citation grounding | Every `[cit:...]` in model output must match an entry in retrieved context; unknown citations stripped during streaming |
| Commander integration | Fire-and-forget child process (`mur conversations compact` spawned by commander's scheduler); opt-out via `enabled_in_daemon=false` |

### Out of scope (Phase 2)

- Multi-turn chat, dashboard UI, RAPTOR, LLMLingua-2, A-MEM dynamic re-linking — all Phase 3.
- Cloud LLM providers (Anthropic/OpenAI). Phase 2 stays strictly local per spec §1 non-goals.
- Real-Ollama CI validation. Phase 2 ships with mocked Ollama tests; real-LLM smoke is `--features ollama-live-smoke` opt-in.

### Explicitly rejected alternatives

- **`mur ask` as `mur conversations ask`** — rejected; `ask` is the daily-driver verb and deserves top-level status alongside `search`.
- **Separate summarizer daemon (`murd`)** — rejected; commander is already running 24/7 for many users and a piggyback trigger is strictly cheaper than a new process with its own config/ops story.
- **Strict §12 append-only with versioned `summary/<date>.vN.md`** — rejected; readers would constantly compute "latest version" and .history/ archive solves the same debug need with less cognitive load.
- **Inline regeneration on `chat show`** — rejected; synchronous LLM call during a read operation is surprising and unbounded-latency. Compact is explicit.

---

## 3. Architecture

### 3.1 Module layout

New code in `mur-core/src/conversations/`:

```
conversations/
├── summarize/              NEW — sleep-time compact
│   ├── mod.rs              public API: compact_day, compact_missing, Summary, CompactReport
│   ├── chunker.rs          split raw day into Ollama-sized chunks (default 6000 tokens)
│   ├── extractive.rs       LLM picks 1-3 spans per chunk; dedupe; cap at N_MAX (default 20)
│   ├── abstractive.rs      LLM narrative over the global span list (150-400 words)
│   ├── macro_refs.rs       Aho-Corasick over ~/.mur/patterns/ names; {{pattern: name}} rewrite
│   └── writer.rs           atomic .md write + .history/ archive + LanceDB layer=1 upsert
├── ask/                    NEW — Mode C
│   ├── mod.rs              public API: ask, ask_stream, AskRequest, AskResponse, AskEvent
│   ├── retrieve.rs         tiered escalation (layer=1 → layer=0), MMR dedup, token-budget cap
│   ├── prompt.rs           system prompt + context assembly + token accounting
│   ├── generate.rs         Ollama /api/generate streaming + temperature/top_p/stop/timeout
│   ├── cite.rs             grounding (strip unknown [cit:...]) + coverage warn heuristic
│   └── format.rs           plain (streaming) + json (buffered) output modes
├── audit.rs                existing; Summarize action variant used by writer.rs
├── index.rs                existing; search() extended with optional layer filter
├── retrieve.rs             existing; Mode A+B unchanged
├── retention.rs            existing; 3-guard unchanged (already respects summary/)
├── store.rs                existing; read_day used by summarize/chunker
└── paths.rs                adds summary_history_dir()
```

New CLI handlers in `mur-core/src/cmd/conversations_cmd.rs`:

- `cmd_conversations_compact(CompactArgs)` — wires `mur conversations compact`.
- `cmd_ask(AskArgs)` — wires top-level `mur ask`.

`mur-core/src/main.rs` gains:

- `Commands::Ask { question, src, since, until, k, model, min_score, json, no_escalate, debug_prompt }`
- `Commands::Conversations::Compact { date, since, force, if_stale, max_days, extractive_only, debug_prompt }`

### 3.2 Data flow

```
                   ┌─────────────────────────────────────────┐
                   │ mur conversations compact                │
                   │  (or commander daemon @ daemon_cron)    │
                   └──────────────┬──────────────────────────┘
                                  ▼
           ┌──────────────────────────────────────────────────┐
           │ for each date in missing_days(throttle=7):        │
           │   1. store::read_day(date)                         │
           │   2. chunker: ~6000-token chunks                   │
           │   3. extractive: LLM → spans (5-20 final)          │
           │   4. abstractive: LLM → narrative paragraph        │
           │   5. macro_refs: pattern-name AC automaton         │
           │   6. frontmatter: counts, sources, keywords        │
           │   7. writer: atomic write + .history/ archive      │
           │   8. audit.append(Summarize{...})                  │
           │   9. index.upsert(summary_embedding, layer=1)      │
           └──────────────────────────────────────────────────┘
                                  ▼
                 summary/<date>.md        index.lance (layer=1 rows)
                         │                       │
                         ▼                       ▼
                   ┌──────────────────────────────────────────┐
                   │ mur ask "<question>"                      │
                   └──────────────┬───────────────────────────┘
                                  ▼
          ┌────────────────────────────────────────────────────┐
          │ ask::ask_stream:                                    │
          │   embed(query) + filters                            │
          │   retrieve: layer=1 k=5 →                           │
          │            if max_score<0.5: layer=0 k=10           │
          │   MMR dedupe (threshold=0.85)                       │
          │   per hit: pull extractive span from summary        │
          │   render_context(~6000 tokens)                      │
          │   ollama.generate_stream(model, prompt)             │
          │   cite.format(tokens) → inline [cit: ...]           │
          │   format.render(plain | json)                       │
          └────────────────────────────────────────────────────┘
```

### 3.3 Invariants

1. **Citations survive retention rotation.** Extractive spans embed the verbatim text plus a pointer. When raw is deleted after `retention_days`, citations still resolve — the snippet lives in the summary.
2. **Compact is idempotent modulo LLM nondeterminism.** Same raw + same model → same summary (extractive stage uses `temperature=0`; abstractive uses `temperature=0.2` — a small divergence in narrative wording is accepted).
3. **Mode C never fabricates.** No retrieved hits above `min_score` → emit explicit "conversations don't cover that." Enforced by prompt instruction and post-generation citation grounding check.
4. **Commander piggyback is opt-in via commander config.** mur operates identically with or without commander running.
5. **Summary is regenerable; raw is not.** §12 narrows: summary overwrite allowed (with `.history/` archive); raw + audit stay strictly append-only.

---

## 4. Compact Pipeline

### 4.1 Chunker

Input: `Vec<Message>` for a date. Output: `Vec<Chunk>` where each chunk fits under `chunk_tokens` (default 6000).

```rust
pub struct Chunk {
    pub messages: Vec<Message>,
    pub token_count: usize,          // chars/4 approximation
    pub span_range: (usize, usize),  // (start_line, end_line) in day-wide JSONL
}

pub fn chunk_day(msgs: &[Message], budget: usize) -> Vec<Chunk>;
```

Rules:

- Never split a single message (quote integrity).
- Prefer splitting at `conv_id` boundaries; split mid-conversation only if that conversation itself exceeds `budget`.
- Token estimate: `chars / 4` — no tokenizer dependency; accurate to ±15% for English/CJK mix.
- Empty day → empty `Vec<Chunk>`; writer emits a minimal "no significant activity" summary.
- Single oversized message → emitted alone; the downstream extractive stage tolerates a truncated Ollama context.

### 4.2 Extractive stage

Per-chunk prompt (`extractive_model`, `temperature=0.0`):

```
You are reviewing one conversation day for a technical developer's personal
archive. Extract the 1-3 most informative spans from this excerpt.

A span is quote-worthy if it:
- states a decision the user made ("we'll use X over Y because...")
- records a concrete error or failure that shaped subsequent work
- captures a new idea, technique, or reference the user hadn't seen before
- quotes an important external fact (API response, spec excerpt, doc)

A span is NOT quote-worthy if it is:
- boilerplate/greeting/filler
- tool-result body already citeable by path
- restated from an earlier span

Output format: JSON array. Each span is {role, conv_id, line_hint, text}.
  - role: one of "user" | "assistant" | "system" | "tool"
  - conv_id: the conv value from the source message
  - line_hint: integer line number within the day's raw JSONL
  - text: verbatim quote, 20-400 chars

If the excerpt has nothing quote-worthy, return [].

Excerpt (<N> messages, lines <span_start>..<span_end>):
<renderMessages(chunk.messages)>
```

Each rendered message: `L<line> [hh:mm:ss] <src>/<conv> (<role>): <text-or-toolref>`.

Per-span validation (silently drop invalid):

- `text` is a verbatim substring of a source message (Jaro-Winkler ≥ 0.95 with a source message's content).
- `line_hint` within the chunk's `span_range`.
- `role` matches the source message's role.

Global dedup + cap:

1. MinHash dedupe at threshold 0.85 (reuse Phase 1's `ingest::dedup`).
2. Stable-sort by `(importance_score, ts)` where importance = length-normalized × role-weight (User=1.2, Assistant=1.0, Tool=0.7, System=0.5).
3. Truncate to `max_extractive_spans` (default 20).

### 4.3 Abstractive stage

Single LLM call (`abstractive_model`, `temperature=0.2`):

```
You are summarizing one day of a developer's AI-assistant conversations into
a narrative paragraph. Use ONLY information present in the spans below.

Output: 150-400 words, first-person or neutral third-person, no bullet lists.
Reference each key point by its span index [N]. Do NOT invent details not in
the spans. If spans conflict, note the conflict.

Spans:
[1] {2026-04-19 slack/bolt-david L4521}: "RaBitQ compresses vectors 32x..."
[2] {2026-04-19 claude-code/3a87 L3344}: "Tool-call pointer substitution..."
[3] ...

Write the narrative.
```

Trailing LLM commentary stripped. On Ollama failure: keep extractive-only summary + frontmatter `warnings: ["narrative_generation_failed"]`.

### 4.4 Macro reference detection

```rust
pub struct MacroRef {
    pub name: String,
    pub pattern_version: u32,
    pub pattern_sha: String,          // SHA-256 of ~/.mur/patterns/<name>.yaml
    pub marker: String,                // "{{pattern: atomic-yaml-write}}"
}

pub fn detect_and_rewrite(
    extractive: &mut [ExtractiveSpan],
    abstractive: &mut String,
    patterns_dir: &Path,
) -> Result<Vec<MacroRef>>;
```

Algorithm:

1. Enumerate `~/.mur/patterns/*.yaml` stems.
2. Build Aho-Corasick automaton over the name set (case-insensitive).
3. Scan extractive span `text` and abstractive narrative for matches (word-boundary enforced in a post-check).
4. Skip matches inside code fences, backticks, or YAML quotes.
5. For each valid match: replace text with `{{pattern: <name>}}`, record one `MacroRef` (dedupe by name).
6. Record `(version, sha)` per referenced pattern — enables future invalidation detection.

### 4.5 Frontmatter schema (exact)

```yaml
---
schema: 1
date: 2026-04-19
generated_at: 2026-04-20T03:00:12Z
generated_by:
  extractive_model: qwen3:14b
  abstractive_model: qwen3:14b
  mur_version: 2.3.0
duration_ms: 18412
conv_count: 12
msg_count: 487
sources: [claude-code, cursor, slack]
pattern_refs:
  - name: atomic-yaml-write
    version: 2
    sha: abc123...
  - name: freedman-compression
    version: 1
    sha: def456...
keywords: [conversations, freedman, compression, ingest]
links:
  prev: ./2026-04-18.md
  next: ./2026-04-20.md
warnings: []
input_content_sha: sha256-of-concatenated-raw-jsonl
---
```

`input_content_sha` is the invalidation key: `--if-stale` compares this against the live raw content; identical → skip.

### 4.6 Body schema

```markdown
## Extractive spans

[1] _{claude-code/3a8786a0 @L3344}_:
> 修一下 yaml.rs 的 atomic write bug

[2] _{slack/C0ARLGHP3A5 @L4521}_:
> LanceDB RaBitQ · 32× 壓縮 · recall@10 差 <1%

## Abstractive narrative

Today's work split into two threads: first, a continued brainstorm of the mur
conversations archive using {{pattern: atomic-yaml-write}} as the writer
model [1]; second, a compression-theory detour into {{pattern: freedman-compression}} [2]...

## Macro expansion map

- {{pattern: atomic-yaml-write}} → patterns/atomic-yaml-write.yaml (v2, sha abc…)
- {{pattern: freedman-compression}} → patterns/freedman-compression.yaml (v1, sha def…)
```

Span IDs `[1]`, `[2]` are stable within the file. Mode C citations reference them via `@summary-span-<N>` suffix.

### 4.7 Writer

```rust
pub fn write_summary(
    date: NaiveDate,
    summary: &Summary,
    root_override: Option<&str>,
) -> Result<()>;
```

Atomic sequence:

1. Render body to `String`.
2. Resolve final path `summary/<date>.md`.
3. If it already exists and content differs from new: move to `summary/.history/<date>.<ISO-8601>.md`.
4. If content is identical to new: return early (no-op, no audit, no history).
5. Write to `summary/.tmp.<date>.md`.
6. Atomic rename tmp → final.
7. Emit `audit::AuditAction::Summarize { date, model, duration_ms }` via `Audit::append`.
8. Compute summary vector embedding; upsert into LanceDB at `layer=1`.

`.history/` cap: keep the `history_retain` most recent versions per date (default 5); older → delete + audit `Delete { reason: "history.rotate" }`.

### 4.8 Orchestrator

```rust
pub async fn compact_missing(cfg: &CompactConfig, root_override: Option<&str>) -> Result<CompactReport>;
pub async fn compact_day(date: NaiveDate, force: bool, cfg: &CompactConfig, root_override: Option<&str>) -> Result<DayReport>;
```

`compact_missing` scans `list_raw_dirs`, filters to `date < today_utc`, skips days with existing summary (unless `--force` or `--if-stale` detects content change), caps at `max_days_per_run`, calls `compact_day` per date. Per-day failures are captured; don't abort the run.

`tracing::info_span!("compact.day", date=%d)` per day.

### 4.9 Failure modes (compact)

| Stage | Fail | Behavior |
|---|---|---|
| Read day | I/O error | Abort this day; report records path + errno |
| Chunk | — | Pure Rust; cannot fail |
| Extract | Ollama timeout/500 | Skip that chunk; continue remaining chunks |
| Extract | Invalid JSON | Zero spans from that chunk; log |
| Dedup/cap | — | Pure Rust |
| Abstractive | Ollama fail | Extractive-only summary + `warnings: ["narrative_generation_failed"]` |
| Macro refs | Pattern YAML unreadable | Skip that pattern; others still inserted |
| Write | Disk/permission | Abort day; staging tmp cleaned on drop |
| Audit | Append fails | Abort day; summary NOT written (invariant: every write has an audit entry) |
| Index | LanceDB fail | Summary IS written (source of truth); rebuild on next `reindex` |

---

## 5. Mode C (Ask)

### 5.1 Public API

```rust
pub struct AskRequest {
    pub question: String,
    pub k_summary: usize,
    pub k_raw: usize,
    pub escalation_threshold: f64,
    pub min_score: f64,
    pub source_filter: Vec<Source>,
    pub since: Option<NaiveDate>,
    pub until: Option<NaiveDate>,
    pub model: String,
    pub format: Format,          // Plain | Json
    pub max_context_tokens: usize,
    pub response_tokens: usize,
    pub timeout: Duration,
    pub no_escalate: bool,
}

pub enum Format { Plain, Json }

pub struct AskResponse {
    pub answer: String,
    pub citations: Vec<Citation>,
    pub hits_used: Vec<HitInfo>,
    pub degraded_to_mode_b: bool,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub duration_ms: u64,
}

pub async fn ask(req: AskRequest, root_override: Option<&str>) -> Result<AskResponse>;
pub async fn ask_stream(req: AskRequest, root_override: Option<&str>)
    -> Result<impl Stream<Item = Result<AskEvent>>>;

pub enum AskEvent {
    Token(String),
    Citation(Citation),
    HitInfo(HitInfo),
    Done { tokens_in: usize, tokens_out: usize, degraded: bool, duration_ms: u64 },
    Error(String),
}

pub struct Citation {
    pub id: u32,
    pub date: NaiveDate,
    pub source: Source,
    pub conv_id: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub snippet: String,
    pub score: f64,
}
```

`ask_stream` is the primary implementation; `ask` buffers over it for `--json`.

### 5.2 Retrieval

```rust
pub async fn gather_hits(
    query: &str,
    filters: &Filters,
    cfg: &AskConfig,
    root_override: Option<&str>,
) -> Result<Vec<Hit>>;
```

Stages:

1. **Embed query** — reuse Phase 1's `embedding::embed`.
2. **Search summaries (layer=1)** — `k_summary` (default 5). Apply since/until/min_score post-filter.
3. **Decision** — if `hits.is_empty() || hits[0].score < escalation_threshold` (default 0.5) → escalate (unless `no_escalate=true`).
4. **Escalate to raw (layer=0)** — `k_raw` (default 10); same post-filters; keep all hits separately from summaries.
5. **MMR dedupe** at `mmr_threshold` (default 0.85, reuses Phase 1 config).
6. **Per-hit snippet resolution** — summary hits: closest extractive span's verbatim text + `span_index`. Raw hits: message text with role prefix, `span_index = None`.
7. **Token-budget cap** — greedy by score until `~90% of max_context_tokens` consumed. Minimum 1 hit always.

`index::ConversationIndex::search` extended with an optional `layer: Option<i8>` parameter (backward-compatible).

### 5.3 Prompt assembly

**Token budgeting** (chars/4 estimator):

```
Fixed:
  system_prompt      ≈ 380 tokens
  question           variable (truncated at 500 tokens with warning)
  response_reserve   cfg.response_tokens (default 1024)
  scaffolding        ≈ 120 tokens
Remaining for context: cfg.max_context_tokens - fixed ≈ 6000 - 2000 = 4000 tokens
```

**System prompt (fixed, ~380 tokens):**

```
You answer questions about the user's past AI-assistant conversations, using
ONLY the excerpts provided below under "Context". Never invent facts not
present in the excerpts.

Every factual claim in your answer MUST be followed by an inline citation
in the form [cit: <date> <source>/<conv_id>:L<line>]. Use only the citations
enumerated in the Context section — one citation per claim. You may use the
same citation multiple times.

If the excerpts are insufficient to answer, say so plainly: "The conversations
I have access to don't cover that." Do not speculate. Do not use training
knowledge to fill gaps.

Format: clear prose, 2-6 sentences per idea, Markdown bullets when listing.
Be direct. Don't repeat the question. Don't apologize for not knowing.

When the user mentions a pattern name wrapped in {{pattern: name}} in the
excerpts, that refers to a reusable artifact at ~/.mur/patterns/<name>.yaml;
you may mention the pattern by name in your answer but do not expand it.
```

**Context format (per hit):**

```
[cit: 2026-04-19 slack/bolt-david:L4521]
> LanceDB RaBitQ · 32× 壓縮 · recall@10 差 <1%

[cit: 2026-04-19 claude-code/3a87:L3344]
> Tool-call pointer substitution 存 sha256+path+bytes 取代內容...

[cit: 2026-04-18 claude-code/c4c2 @summary-span-3]
> 我決定把 commander audit chain 用 bridge 方式延續...
```

Citations use `@summary-span-<N>` when the hit is a layer=1 summary span; raw hits use `:L<line>`.

### 5.4 Generation & streaming

Ollama `/api/generate` with `stream=true`:

```rust
pub async fn generate_stream(
    model: &str,
    system: &str,
    user: &str,
    endpoint: &str,
    timeout: Duration,
) -> Result<impl Stream<Item = Result<String>>>;
```

Parameters:

- `temperature: 0.1`
- `top_p: 0.9`
- `num_predict: cfg.response_tokens`
- `stop: ["\n\nQ:", "\n\nQuestion:"]`

Stream consumer:

- Maintain a 64-char tail buffer to detect `[cit:...]` spans emerging across chunk boundaries.
- On detecting a closed bracket: parse + validate against context citation list; unknown → strip silently + log warn.
- Re-emit validated tokens to caller's stream.

**Timeout:** wall-clock `cfg.timeout_secs` (default 120). On hit: close stream, `Err(Timeout)`; partial output has already reached the caller.
**Stall detection:** if > 20s elapses with no token, abort with partial output + warn.

### 5.5 Citation grounding

Two checks:

**1. Grounding.** Every `[cit: ...]` in output must match the context's citation list. Unknown → strip during streaming.

**2. Coverage (soft).** After generation, count claim-like sentences lacking adjacent citations. Heuristic:

- Split answer into sentences.
- "Claim" = sentence with a content verb (`is`/`was`/`means`/`uses`/`adopts`/`rejects`/`shows`/etc.) OR containing a specific name/number.
- If `claim_without_cite_ratio > 0.3`: append a warning line to the output (`⚠ Some claims above are not cited; treat them as model synthesis, not archive content.`). Does not reject output — Phase 2 is warn-only; Phase 3 can add strict mode.

### 5.6 Output formatting

**Plain (default):**

- Stream tokens to stdout.
- After stream completes:
  - Blank line
  - `Citations:` header
  - Per unique citation used: `[cit: ...] — <snippet first 120 chars>` lines, ordered by first reference in answer
  - Runtime footer: `(<N> hits · <ms>ms · <tokens_in>→<tokens_out> tokens[ · Mode B fallback])`

**JSON (`--json`):**

Buffered full response as AskResponse; emit once at end. Streaming JSON is unparseable mid-stream, so `--json` mode gives up streaming for structural validity.

### 5.7 Failure degradation

| Failure | Behavior | Exit |
|---|---|---|
| Ollama unreachable | Degrade to Mode B; `[LLM unavailable] Here are the top N relevant excerpts:` + snippets | 0 |
| Ollama 5xx/timeout | Same as unreachable | 0 |
| Model not pulled | `error: Ollama doesn't have <model>. Run 'ollama pull <model>'` | 1 |
| No vector index | `error: no index. Run 'mur conversations reindex'` | 1 |
| No hits above min_score | `The conversations I have access to don't cover that.` + note on filters/retention | 0 |
| Context oversize at k=1 | Truncate single snippet; emit warning | 0 |
| Stream stall > 20s | Abort + partial output + warn | 1 |

### 5.8 CLI surface

```bash
mur ask <question>                            # streaming plain text
mur ask <question> --json                     # structured JSON
mur ask <question> --src claude-code,slack    # filter sources
mur ask <question> --since 2026-04-01         # time filter
mur ask <question> --k 8                      # override top-k (sets k_raw = k * 2)
mur ask <question> --model qwen3:32b          # override model
mur ask <question> --no-escalate              # skip layer=0 fallback
mur ask <question> --debug-prompt             # print rendered prompt to stderr
```

`mur ask` lives at the top level (not nested under `conversations`) — it's the daily-driver verb and should feel as light as `mur search`.

---

## 6. Config Schema

### 6.1 `mur-common::config::ConversationsConfig` extensions

```rust
pub struct ConversationsConfig {
    // Phase 1 fields unchanged.
    pub enabled: bool,
    pub retention_days: u32,
    pub poll_interval_secs: u64,
    pub sources: ConversationsSources,
    pub filter:  ConversationsFilter,

    // Phase 2 additions.
    pub compact: CompactConfig,
    pub ask:     AskConfig,
}

pub struct CompactConfig {
    pub enabled_in_daemon: bool,        // default true
    pub max_days_per_run: u32,          // default 7
    pub extractive_model: String,       // default "qwen3:14b"
    pub abstractive_model: String,      // default "qwen3:14b"
    pub ollama_endpoint: String,        // default "http://localhost:11434"
    pub max_extractive_spans: u32,      // default 20
    pub max_abstractive_words: u32,     // default 400
    pub chunk_tokens: u32,              // default 6000
    pub history_retain: u32,            // default 5
    pub daemon_cron: String,            // default "0 3 * * *" (03:00 local)
}

pub struct AskConfig {
    pub model: String,                  // default "qwen3:14b"
    pub ollama_endpoint: String,        // default "http://localhost:11434"
    pub k_summary: u32,                 // default 5
    pub k_raw: u32,                     // default 10
    pub escalation_threshold: f64,      // default 0.5
    pub mmr_threshold: f64,             // default 0.85
    pub max_context_tokens: u32,        // default 6000
    pub response_tokens: u32,           // default 1024
    pub timeout_secs: u32,              // default 120
    pub min_score: f64,                 // default 0.35
}
```

All fields `#[serde(default)]` — existing Phase 1 `config.yaml` files continue to parse.

### 6.2 P4 auto-sync extension

Phase 1's P4 sync (`migrate.rs::sync_commander_config_toml`) writes a marker-delimited `[conversations]` block into `~/.mur/commander/config.toml`. Phase 2 extends the synced subset:

```toml
# BEGIN [conversations] (managed by mur conversations migrate)
[conversations]
enabled = true
retention_days = 30

[conversations.compact]
enabled_in_daemon = true
daemon_cron = "0 3 * * *"
# END [conversations]
```

The new `[conversations.compact]` sub-section is added to the sync writer. Everything else Phase 1 synced stays.

### 6.3 Full config example

```yaml
conversations:
  enabled: true
  retention_days: 30
  poll_interval_secs: 300
  sources:
    claude_code: true
    cursor: true
    gemini: true
    aider:
      enabled: true
      watched_dirs: ["~/Projects"]
  filter:
    dedup_threshold: 0.85
    reject_heartbeat: true
    reject_system_restatement: true
  compact:
    enabled_in_daemon: true
    max_days_per_run: 7
    extractive_model: qwen3:14b
    abstractive_model: qwen3:14b
    ollama_endpoint: http://localhost:11434
    max_extractive_spans: 20
    max_abstractive_words: 400
    chunk_tokens: 6000
    history_retain: 5
    daemon_cron: "0 3 * * *"
  ask:
    model: qwen3:14b
    ollama_endpoint: http://localhost:11434
    k_summary: 5
    k_raw: 10
    escalation_threshold: 0.5
    mmr_threshold: 0.85
    max_context_tokens: 6000
    response_tokens: 1024
    timeout_secs: 120
    min_score: 0.35
```

---

## 7. Commander Piggyback

Commander has a long-running daemon with a scheduler; Phase 2 reuses that instead of introducing a new process.

### 7.1 New trigger: `ConversationsCompactTrigger`

New file: `mur-commander/crates/daemon/src/triggers/conversations_compact.rs`.

```rust
pub struct ConversationsCompactTrigger {
    cron: cron::Schedule,
    last_fired: Mutex<Option<DateTime<Utc>>>,
}

impl Trigger for ConversationsCompactTrigger {
    fn name(&self) -> &'static str { "conversations_compact" }

    async fn tick(&self, now: DateTime<Utc>) -> Result<TriggerAction> {
        let last = *self.last_fired.lock().unwrap();
        let Some(next) = self.cron.after(&last.unwrap_or(now - Duration::days(1))).next() else {
            return Ok(TriggerAction::None);
        };
        if now >= next {
            *self.last_fired.lock().unwrap() = Some(now);
            Ok(TriggerAction::Exec(ExecSpec {
                command: "mur".into(),
                args: vec!["conversations".into(), "compact".into()],
                timeout_secs: 600,           // 10 min
                capture_output: true,
            }))
        } else {
            Ok(TriggerAction::None)
        }
    }
}
```

### 7.2 Design rationale

1. **Fire-and-forget child process, not in-process.** Isolates commander from Ollama hangs; 10-min timeout kills the child cleanly. Users debug by running the same CLI by hand. No dep coupling between mur and commander versions.
2. **Cron lives in commander config.** `conversations.compact.daemon_cron` in mur's `config.yaml` mirrored to commander's toml via P4 sync. Commander reads only its own toml at runtime.
3. **Audit to commander's chain.** Trigger start + result logged via commander's `AuditStore` (workflow-oriented schema). Doesn't touch mur's conversations audit chain — the child `mur` process writes its own `Summarize` entry there.
4. **Opt-out.** `enabled_in_daemon=false` in commander's toml → trigger skips registration at daemon start.
5. **Overlap prevention.** If a previous exec is still running when the trigger fires, skip this tick. Phase 1's `.pull.lock` flock further prevents pipeline interleaving.

### 7.3 Commander-side changes

- `crates/daemon/src/triggers/conversations_compact.rs` — new (~80 LOC)
- `crates/daemon/src/triggers/mod.rs` — register new trigger
- `crates/daemon/src/main.rs` — read cron from commander config, instantiate trigger, add to scheduler
- `crates/daemon/src/config.rs` — add `ConversationsCompactConfig` struct matching the toml block

### 7.4 `mur conversations doctor` updates

Phase 1's doctor gains Phase 2 checks:

```
conversations doctor
  ✓ raw day-dirs: 12
  ✓ audit hash chain
  ✓ retention_days = 30
  ✓ conversations.enabled
  ✓ summaries: 11 of 12 present  (missing: 2026-04-20 today; run 'mur conversations compact')
  ✓ Ollama reachable at http://localhost:11434
  ✓ model qwen3:14b available
  ✓ compact config valid
  ✓ ask config valid
  ✓ commander compact trigger: enabled (next fire 2026-04-21T03:00:00-07:00)
```

All checks informational — failures warn but don't exit non-zero (unless an explicitly blocking issue like `conversations.enabled=false`).

### 7.5 `mur conversations preflight` updates

Phase 1's preflight covered migration safety. Phase 2 adds checks before the first compact-or-ask:

- Ollama reachable?
- Models `compact.extractive_model`, `compact.abstractive_model`, `ask.model` all pulled?
- Pattern directory `~/.mur/patterns/` readable?
- Free memory ≥ 4 GB (Linux/macOS via `sysinfo` crate)?
- Disk free for `.history/` growth over 30 days (estimate: N_summaries × avg 4 KB)?

Non-blocking; warnings only.

---

## 8. Amendments to Phase 1

### 8.1 §12 append-only narrowing

**Add to Phase 1 spec §12 as the final paragraph of the section:**

> **Phase 2 narrowing (2026-04-20 amendment).** The append-only guarantee applies strictly to source data (`raw/<date>/*.jsonl`) and the audit chain (`audit.jsonl`). Summaries (`summary/<date>.md`) are derived artifacts and MAY be overwritten by a newer compact run (via `mur conversations compact --force` or a newer prompt/model version). On overwrite, the previous summary is moved to `summary/.history/<date>.<ISO-8601>.md`; the most recent `history_retain` versions per date are kept, older ones are deleted with an audit `Delete{reason:"history.rotate"}` entry. This narrowing lets prompt/model improvements reach existing users without requiring destructive opt-in migrations.

### 8.2 §6.5 concretization

**Add to Phase 1 spec §6.5 as a prepended note:**

> **Phase 2 replaces this sketch with a concrete algorithm; see `docs/superpowers/specs/2026-04-20-mur-conversations-phase-2-design.md` §5.**

The original §6.5 pseudocode remains as historical context.

### 8.3 §6.7 REST API stubs

Phase 1 §6.7 listed stub endpoints for a future dashboard. Phase 2 does NOT implement the REST API — Mode C stays CLI-only. Phase 3 implementation can use the same `AskResponse` / `AskEvent` types as the CLI and just wrap them in an HTTP/SSE transport. The types are designed for that reuse already.

---

## 9. Testing Strategy

### 9.1 Unit tests (per module, mocked Ollama)

| Module | Key tests |
|---|---|
| `summarize/chunker.rs` | budget-respecting pack; single-msg-over-budget falls through; conv-boundary preference; empty day → zero chunks |
| `summarize/extractive.rs` | valid JSON → validated spans; Jaro-Winkler verbatim check catches paraphrases; invalid role rejected; line_hint range check; LLM empty array → zero spans |
| `summarize/abstractive.rs` | happy path; Ollama 500 → extractive-only warning set; 400-word cap respected; trailing commentary stripped |
| `summarize/macro_refs.rs` | Aho-Corasick happy path; word-boundary enforcement (pattern `rust` doesn't match `rustic`); code-fence / backtick skip; no-patterns case (empty patterns/ dir) |
| `summarize/writer.rs` | atomic rename; .history/ archive on overwrite; identical summary → no-op; .history/ cap; audit Summarize entry appended; LanceDB upsert roundtrip |
| `ask/retrieve.rs` | layer-1 path when top score ≥ 0.5; escalation to layer-0 when below; MMR dedup threshold; token-budget cap; filter combinators (src+since+until) |
| `ask/prompt.rs` | token accounting accurate to ±5%; oversized question truncation + warn; scaffolding overhead constant; citation anchors unique per hit |
| `ask/generate.rs` | stream yields tokens in order; timeout aborts cleanly; stall detection; temp/top_p/stop params honored |
| `ask/cite.rs` | grounding drops unknown `[cit:...]`; coverage warn threshold 30%; `@summary-span-<N>` variant parsed; claim-sentence heuristic |
| `ask/format.rs` | plain format byte-matches spec example (modulo timestamps); `--json` produces valid JSON; AskEvent round-trip |

### 9.2 Integration tests (wiremock'd Ollama)

- `compact_e2e`: seed raw day → `compact_day` → assert summary.md has valid frontmatter, spans, narrative, LanceDB layer=1 row, audit Summarize entry.
- `compact_force_archives`: compact twice (different mock responses) → .history/ has 1 prior version.
- `compact_if_stale_noop`: raw unchanged + summary present → no work.
- `ask_summary_hit`: seed 3 days with summaries → ask question matching one summary → cite only that summary's spans.
- `ask_escalates_to_raw`: mock retrieval returns low-score summary hits → assert raw-layer search fires.
- `ask_degrades_on_ollama_down`: wiremock returns connection-refused → `degraded_to_mode_b=true`, no panic.
- `ask_rejects_fabricated_cite`: mock Ollama emits `[cit: 2099-01-01 fake:L0]` → grounding filter strips it.

### 9.3 Golden-path script update

Extend `scripts/golden-path-conversations.sh` with two new steps:

```bash
# Step 9: compact
MUR_OLLAMA_MOCK=1 "$MUR" conversations compact --date "$TODAY" | tee /tmp/gp-out-9.txt
grep -q "1 ok, 0 failed" /tmp/gp-out-9.txt || { echo "FAIL"; exit 1; }
[[ -f "$TMPHOME/.mur/conversations/summary/$TODAY.md" ]] || { echo "FAIL"; exit 1; }

# Step 10: ask
MUR_OLLAMA_MOCK=1 "$MUR" ask "what compression techniques did I discuss" --json | tee /tmp/gp-out-10.txt
jq -e '.citations | length > 0' /tmp/gp-out-10.txt >/dev/null || { echo "FAIL"; exit 1; }

echo "=== ALL 10 STEPS GREEN ==="
```

`MUR_OLLAMA_MOCK=1` is a new test helper: the Ollama client checks this env and returns canned responses (deterministic extractive spans, fixed abstractive narrative, fixed ask answer with one valid citation). Keeps the golden path deterministic and offline.

### 9.4 Real-Ollama smoke (feature-gated)

```bash
cargo test -p mur-core --features ollama-live-smoke -- --ignored
```

Runs actual `qwen3:14b` against a seeded day; CI skips this (needs Ollama + 8 GB model); local devs run when tuning prompts.

---

## 10. Operational Details

### 10.1 Error handling cross-reference

| Source | Fail | User CLI outcome | Audit | Exit |
|---|---|---|---|---|
| compact / read day | I/O | `✗ <date>: <err>` | `Error{stage:"compact.read"}` | 0 (partial OK) |
| compact / extract | Ollama 5xx | per-chunk skip + warn | — | 0 |
| compact / extract | invalid JSON | warn + zero spans that chunk | — | 0 |
| compact / abstractive | Ollama fail | summary has `warnings:["narrative_generation_failed"]` | `Summarize{..., warnings}` | 0 |
| compact / write | disk full | `✗ <date>: no space` | `Error{stage:"compact.write"}` | 1 |
| compact / audit append | chain broken | abort day, summary NOT written | (next startup detects) | 1 |
| ask / Ollama unreachable | connect refused | Mode B fallback, full snippets | — | 0 |
| ask / Ollama error | 500/timeout | Mode B fallback + warn | — | 0 |
| ask / model missing | not found | `error: ollama pull <model>` | — | 1 |
| ask / no index | empty LanceDB | `error: run 'mur conversations reindex'` | — | 1 |
| ask / zero hits | retrieval empty | "conversations don't cover that" | — | 0 |
| ask / grounding | fake `[cit:...]` | strip silently, log warn | — | 0 |
| ask / stream stall | > 20s quiet | abort + partial output + warn | — | 1 |

### 10.2 Observability

All new code wrapped in `tracing::info_span!` per Phase 1's BP6 pattern:

- `conversations.compact.day(date=%d)` — covers all stages of one day's compact
- `conversations.compact.extractive(chunk_idx=%u)` — per chunk
- `conversations.compact.abstractive(span_count=%u)` — the single abstractive call
- `conversations.compact.write(date=%d)` — atomic write + audit + index
- `conversations.ask.retrieve(k=%u)` — retrieval stages
- `conversations.ask.generate(model=%s)` — LLM streaming
- `conversations.ask.cite(cite_count=%u)` — grounding + coverage checks

Enable with `RUST_LOG=mur_core::conversations=debug`.

### 10.3 Phase 2 rollout order

**Phase 2A — compact only** (shippable independently):

- Sections 3 (module layout) + 4 (compact pipeline) + 6.1/6.2/6.3 (compact config + P4 sync) + 7 (commander trigger) + §8.1 amendment + relevant tests.
- **Value without ask:** users can finally retain conversations forever via daily summaries. Retention 3-guard starts deleting raw past `retention_days` cleanly.
- **PR scope:** mur repo + mur-commander repo.

**Phase 2B — ask** (depends on 2A):

- Sections 3 (ask module) + 5 (ask implementation) + 6.1 (ask config) + relevant tests.
- **Value:** users can ask natural-language questions about their archive.
- **PR scope:** mur repo only.

**Phase 2C — hardening** (can slip to Phase 2.1):

- Citation grounding strict mode
- Real-Ollama smoke in CI (once CI runners support it or we formalize the mock)
- `.history/` cap cleanup job + audit
- `--debug-prompt` polish

Each sub-phase gets its own PR. Merge order: 2A → 2B → 2C.

### 10.4 Phase 3 items explicitly DEFERRED

- Multi-turn chat (`mur ask --continue`)
- mur-web `/conversations` dashboard
- LLMLingua-2 pre-summarization
- RAPTOR week/month summaries (layer=2+)
- A-MEM dynamic re-linking (pattern SHA drift detection)
- Cloud LLM provider support (Anthropic/OpenAI) — if ever

### 10.5 Success criteria (acceptance)

Phase 2 ships when:

- ✅ `mur conversations compact --date YYYY-MM-DD` produces a valid summary matching §4.5/4.6 schema.
- ✅ `mur conversations compact` catches up missing days with throttle.
- ✅ `mur ask <question>` emits streaming plain-text answer with ≥1 grounded citation OR clean "nothing matched" message.
- ✅ `mur ask <question> --json` emits valid JSON matching `AskResponse` schema.
- ✅ Retention 3-guard allows raw deletion at T+30d after compact runs.
- ✅ Commander daemon fires `mur conversations compact` on schedule when enabled.
- ✅ Ollama unreachable → ask degrades to Mode B without panic.
- ✅ Golden path script passes all 10 steps with `MUR_OLLAMA_MOCK=1`.
- ✅ Clippy + fmt clean on workspace.
- ✅ `cargo test --workspace` passes (excluding feature-gated real-Ollama tests).

---

## 11. Compatibility Constraints (Hard)

Non-negotiable; every implementation choice must honor these:

1. **Raw + audit stay append-only.** Only summary/.md may be overwritten (with .history/ archive). Spec §12 narrowing applies only to summaries.
2. **Mode C must not fabricate citations.** Unknown `[cit:...]` stripped at streaming time. Zero exceptions.
3. **Ollama unreachable must not crash ask.** Degrade to Mode B; return a clear "LLM unavailable" message.
4. **Summary frontmatter schema is forward-versioned.** `schema: 1` field present on every write; readers must handle missing fields via serde-default (the amended summary schema uses `#[serde(default)]` throughout).
5. **Commander piggyback is OPTIONAL.** mur operates identically whether commander is running or not.
6. **No Phase 3 leakage.** Don't implement multi-turn, dashboard, LLMLingua, RAPTOR, or A-MEM features "while we're there." Each has its own spec cycle.

---

## 12. Open Questions

None blocking Phase 2A or 2B implementation. Flagged for Phase 3 discussion:

- Should `ask` support cross-repo context (mix pattern content with conversation content in prompts)? The macro-expansion path makes this trivially possible but the default stance in Phase 2 is "patterns referenced by name, not expanded inline."
- Should compact store summary embedding at multiple resolutions (`layer=1` full summary + `layer=1.5` per-section)? Phase 3 RAPTOR makes this a structural decision.
- Should Mode C offer `--verbose` that shows retrieval scores and prompt assembly inline? Phase 2 keeps `--debug-prompt` as a focused debug switch; UI polish for observability is Phase 3.

---

## 13. References

- Phase 1 spec: `docs/superpowers/specs/2026-04-19-mur-conversations-design.md`
- Phase 1 plan: `docs/superpowers/plans/2026-04-19-mur-conversations-phase-1.md`
- Phase 1 amendments: `docs/superpowers/plans/2026-04-19-mur-conversations-phase-1-amendments.md`
- Shipped PRs: mur-run/mur#5 (squash `4664a4c`), mur-run/mur-commander#9 (squash `3a5bf1b`)
- Freedman, Aksenov, Bodnia, Mulligan. *Compression is all you need: Modeling Mathematics.* arXiv 2603.20396 (2026-03-20) — Named-Abstraction theory underpinning macro-expansion (§4.4, §4.6).
- Mem0 production audit. GitHub issue #4573. — 97.8% junk finding; informs "extract only quote-worthy" framing in §4.2.
- MemGPT tiered retrieval. arXiv 2310.08560. — layer-1 → layer-0 escalation pattern adopted in §5.2.
- Perplexity citation format — informed the inline `[cit:...]` style in §5.3.

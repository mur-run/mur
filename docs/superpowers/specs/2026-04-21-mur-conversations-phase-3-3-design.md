# mur Conversations Phase 3.3 — Multi-turn `mur ask --continue` Design

**Status:** Approved 2026-04-21
**Depends on:** Phase 3.2 shipped (merge commit `e207b09` — week/month rollups + collapsed-tree retrieval)
**Phase 3.4+ upgrade path:** HyDE / multi-query rewriting (conditional on retrieval confidence); LLM-driven summarization of dropped turns; named sessions via `--session <id>`.

---

## 1. Purpose

Mode C `mur ask` (shipped in Phase 2B) is one-shot: each invocation is an independent Q&A pair with no knowledge of prior queries. Phase 3.3 adds conversational memory across invocations so users can ask follow-up questions ("what about the week before?", "elaborate on that third citation") without re-stating full context each time.

The implementation matches the 2025-2026 industry consensus for conversational RAG: JSONL session files (Claude Code, Gemini CLI, OpenAI Agents SDK pattern), LangChain-style "condense question" rewriting before retrieval, and a small fixed rolling window of prior turns fed into the generation prompt.

## 2. Non-goals

Explicitly deferred to future phases or declined:

- **Named / parallel sessions** (`--session <id>`, `--session-list`). Only one active session file at a time.
- **HyDE or multi-query rewriting.** Production cost is prohibitive on consumer Ollama (25-60% latency increase per turn, best results require 5 generations). Deferred to Phase 3.4 if measured hit-quality justifies it.
- **LLM-driven memory summarization** (MemGPT / A-MEM style). Rolling window of last 3 turns verbatim is sufficient for the v1 target — no summarization path, no LLM tool calls for memory management.
- **Per-project session scoping** (hash of `$PWD`). Misaligned with the global conversations archive's semantics; Claude Code does this because its queries target a specific repo, but mur's archive is cross-project.
- **Idle auto-rotation.** Users explicitly start fresh sessions; no silent "session expired" magic.
- **Eager summarization of answers for the rewriter prompt.** Answers are truncated to 500 chars instead.

## 3. Architecture

```
mur ask [--continue | --new | --show-session] "question" [flags...]
  │
  ├── --show-session path (no LLM calls) ──────────→ print session summary, exit
  │
  ├── SessionStore (new: ask/session.rs)
  │     ├─ load_latest() -> Session              (used by --continue)
  │     ├─ archive_and_new() -> Session          (default / --new)
  │     └─ append_turn(&mut Session, TurnRecord) (after successful or degraded turn)
  │
  ├── Query rewrite (ask/rewriter.rs) — only if --continue AND prior_turns non-empty
  │     └─ rewrite(client, model, prior_turns, raw_q) -> RewriteResult { rewritten, status }
  │
  ├── gather_hits(rewritten_q)  — unchanged from Phase 3.2 collapsed tree
  │
  └── Generate (prompt::render extended with prior_turns context)
        └─ Returns AskResponse → append TurnRecord to session JSONL (crash-safe)
```

### 3.1 Module structure

| File | Role | Status |
|---|---|---|
| `mur-core/src/conversations/ask/session.rs` | `SessionStore`, `TurnRecord`, `RewriterStatus` | **New** (~200 LOC) |
| `mur-core/src/conversations/ask/rewriter.rs` | `rewrite()` + `CONDENSE_PROMPT` + mock branch | **New** (~100 LOC) |
| `mur-core/src/conversations/ask/mod.rs` | Register submodules; extend `AskRequest` / `AskResponse` | Modify |
| `mur-core/src/conversations/ask/prompt.rs` | `render` accepts `prior_turns: &[TurnRecord]`; new history section | Modify |
| `mur-core/src/conversations/ask/generate.rs` | Pass session ref; append TurnRecord on success + degraded path | Modify |
| `mur-core/src/conversations/paths.rs` | `ask_session_path`, `ask_session_history_dir` | Modify |
| `mur-core/src/conversations/ollama.rs` | Mock branch for `"Standalone question:"` identity rewrite | Modify |
| `mur-core/src/cmd/conversations_cmd.rs` | `AskArgs` + `cmd_ask` extended with continue/new/show_session | Modify |
| `mur-core/src/main.rs` | `Ask` variant gets 3 new flags with clap mutex/exclusive | Modify |
| `mur-common/src/config.rs` | `AskConfig.continue_history_turns: u32 = 3` | Modify |
| `mur-core/tests/cli_conversations.rs` | 4 integration tests | Modify |
| `scripts/golden-path-conversations.sh` | Steps 16 & 17; banner 15 → 17 | Modify |

No new Cargo dependencies. No LanceDB schema changes. No commander sync (`[conversations.rollup]` block unchanged).

### 3.2 Tech stack

Rust 2024 · tokio · serde_json (already used) · chrono · existing Phase 3.2 deps.

## 4. Session format & TurnRecord

### 4.1 File location

- Active: `~/.mur/conversations/ask-session.jsonl`
- Archive: `~/.mur/conversations/ask-sessions/.history/<utc-timestamp>.jsonl`
- Retention: reuse `ConversationsConfig.compact.history_retain` (default 5). `.history/` pruned on each `--new` / default-archive operation, matching Phase 2C's `prune_history` window-scoped pattern.

### 4.2 Wire format

JSON Lines, one `TurnRecord` per line, append-only. No outer wrapper. Crash-safe per industry convention.

### 4.3 TurnRecord schema

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub v: u32,                           // schema version = 1
    pub turn_id: u32,                     // 1-indexed within session
    pub ts: DateTime<Utc>,                // turn start time
    pub question: String,                 // raw user input
    pub rewritten_question: Option<String>, // Some iff rewriter ran
    pub hits_used: Vec<HitInfo>,          // same shape as AskResponse.hits_used
    pub answer: String,                   // "" iff degraded_to_mode_b
    pub citations: Vec<Citation>,         // per-turn [1..N], never cross-turn
    pub degraded_to_mode_b: bool,
    pub rewriter_status: RewriterStatus,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RewriterStatus {
    Skipped,               // turn 1 OR --continue not passed OR session empty
    Rewrote,               // rewriter emitted a differing standalone question
    NoRewriteNeeded,       // rewriter echoed verbatim (LangChain "return as is" fallback)
    FailedFellBackToRaw,   // Ollama error on rewrite; retrieval used raw question
}
```

**Rationale:**
- `v` — schema-version field for future migration safety. Loader handles unknown versions by skipping with `tracing::warn!`.
- `turn_id` — explicit 1-indexed ordering for `--show-session` UX and deterministic replay.
- `rewritten_question: Option<String>` — `None` when rewriter didn't run (`Skipped`); `Some(...)` for the three other statuses. Visible in `--json` output for transparency.
- Citations stored per turn but **never re-cited across turns** (see §5.3).
- `degraded_to_mode_b` + `rewriter_status` — explicit failure-trail for the graceful-degrade path (§6).

### 4.4 Session loading semantics (`--continue`)

1. Open `ask-session.jsonl`. If missing → error with message: `"no prior session; run without --continue to start a new one"`. Non-zero exit.
2. Read line-by-line. For each line:
   - Parse as `TurnRecord`. On error, emit `tracing::warn!` with turn index + reason, skip the line. Never fail the whole load on one bad line.
   - On success, push to in-memory vec.
3. If resulting vec is empty (file exists but all lines malformed, or file is empty) → error as in step 1.
4. Else → return `Session { turns }`.

### 4.5 Archive-and-new semantics (default / `--new`)

1. If `ask-session.jsonl` exists and is non-empty:
   - Atomically rename to `ask-sessions/.history/<UTC>.jsonl` via `std::fs::rename`.
   - Invoke `prune_history_in(history_dir, history_retain)` (extract or reuse the Phase 2C / Phase 3.2 pattern).
2. If it's empty or missing: no-op.
3. Return an empty in-memory `Session` (file creation deferred to first `append_turn`).

### 4.6 Append semantics

1. Open `ask-session.jsonl` with `OpenOptions::new().create(true).append(true)`.
2. Serialize `TurnRecord` → JSON (compact, single line).
3. Write `json_bytes + b"\n"`.
4. `file.sync_all()` to flush to disk before returning (matches Phase 2 writer.rs pattern; guarantees crash durability of the turn).

## 5. Query rewriting

### 5.1 Prompt

Verbatim LangChain canonical "condense question" prompt, stored as a module-level constant:

```rust
const CONDENSE_PROMPT: &str = "Given a chat history and the latest user question \
    which might reference context in the chat history, formulate a standalone \
    question which can be understood without the chat history. Do NOT answer \
    the question, just reformulate it if needed and otherwise return it as is.\n\n\
    Chat history:\n{history}\n\n\
    Latest question: {question}\n\n\
    Standalone question:";
```

Where `{history}` is rendered:
```
User: {q_1}
Assistant: {a_1_truncated_to_500_chars}
User: {q_2}
Assistant: {a_2_truncated_to_500_chars}
...
```

### 5.2 Ollama options

```rust
GenerateOptions {
    temperature: Some(0.1),     // deterministic-ish (rewrites should be stable)
    top_p: Some(0.9),
    num_predict: Some(80),       // rewrites are short
    stop: vec!["\n".into()],     // cut at first newline
}
```

### 5.3 Public API

```rust
pub struct RewriteInput<'a> {
    pub prior_turns: &'a [TurnRecord],  // trimmed to last N by caller
    pub raw_question: &'a str,
}

pub struct RewriteResult {
    pub rewritten: String,          // always non-empty; == raw_question iff no rewrite
    pub status: RewriterStatus,
}

pub async fn rewrite(
    client: &OllamaClient,
    model: &str,
    input: RewriteInput<'_>,
) -> RewriteResult;
```

### 5.4 Failure modes

| Condition | Status | Resulting `rewritten` |
|---|---|---|
| `prior_turns.is_empty()` | `Skipped` (caller-handled, no LLM call) | `raw_question` |
| Ollama timeout / connection error | `FailedFellBackToRaw` | `raw_question` |
| Empty LLM response | `FailedFellBackToRaw` | `raw_question` |
| Response (trimmed, case-insensitive) == raw | `NoRewriteNeeded` | `raw_question` |
| Otherwise | `Rewrote` | trimmed LLM response |

### 5.5 Mock integration (`MUR_OLLAMA_MOCK=1`)

Extend `mock_generate` with a new branch matched on `"Standalone question:"` substring:

```rust
} else if req.prompt.contains("Standalone question:") {
    // Identity rewrite — matches LangChain "return as is" fallback.
    // Tests asserting rewrite happened should use MUR_OLLAMA_MOCK=hash.
    extract_latest_question_from_prompt(req.prompt).to_string()
}
```

`extract_latest_question_from_prompt` is a small module-level helper that finds `"Latest question: "`, returns the substring up to the next `\n\n`.

## 6. Generation integration

### 6.1 Prompt render signature

```rust
pub fn render(
    question: &str,                // raw user question (not rewritten)
    prior_turns: &[TurnRecord],    // oldest → newest, already trimmed to N
    hits: &[ResolvedHit],
    max_context_tokens: usize,
    response_tokens: usize,
) -> RenderedPrompt;
```

### 6.2 Prompt structure

```
[SYSTEM]
<existing system prompt, unchanged>

[CHAT HISTORY]                          ← new, present only if prior_turns non-empty
User: q_1
Assistant: a_1 (truncated to ~500 chars)
User: q_2
Assistant: a_2 (truncated to ~500 chars)
...

[CONTEXT HITS]                          ← existing format, unchanged
[1] <hit>
[2] <hit>
...

[USER QUESTION]
<original raw question>                 ← NOT the rewritten one
```

**Why raw question in generation:** the rewritten query is solely for retrieval. If it appeared in the generation prompt, users watching streaming output would see "what did I ship the week of 2026-W14?" when they typed "what about the week before?" — confusing.

### 6.3 Citations

- Each turn renders fresh `[1], [2], ...` based only on the current turn's `hits_used`.
- Prior-turn citations appear in the history section as plain text (embedded in the prior `answer` string) and are **not** re-numbered or resolved.
- The JSONL stores prior-turn citations for audit/replay; they're just not surfaced in the user-visible output of subsequent turns.

### 6.4 Budget management

Existing logic handles overall `max_context_tokens` via hit-shrinking in `render_shrinks_hits_on_overflow`. Extended rules:

1. Start with: system prompt + raw question + hits + chat history section.
2. If over budget → drop **oldest history turn first**, then next-oldest, etc.
3. If history fully dropped and still over → fall through to existing hit-shrinking.

Rationale (backed by Chroma "Context Rot" research 2025): hits matter more than distant history for answer quality. Oldest turns drop first.

### 6.5 Graceful degradation (Q6 decision)

| Stage | Failure action | Session write |
|---|---|---|
| Rewriter fails | Use `raw_question` for retrieval; set `status = FailedFellBackToRaw` | Yes, turn persisted |
| Generation fails | Mode B (snippets-only) fallback as in Phase 2B; `answer = ""` (or formatted snippets), `degraded_to_mode_b = true` | Yes, turn persisted |
| Both fail | Rewriter falls back to raw; generation returns Mode B | Yes, turn persisted |
| Session load fails (corrupt JSONL beyond skip-line threshold) | Error before LLM calls; no turn written | N/A |

All partial-failure turns are written to the JSONL so `--show-session` surfaces them and `--continue` remains consistent.

## 7. CLI surface

### 7.1 Flags

```rust
Ask {
    question: Option<String>,

    /// Append to the current session (multi-turn mode).
    #[arg(long, conflicts_with = "new")]
    r#continue: bool,

    /// Archive current session and start fresh (default behavior; flag is explicit for scripts).
    #[arg(long, conflicts_with = "continue")]
    new: bool,

    /// Print current session path, turn count, last-turn time, first-question preview. No LLM calls.
    #[arg(long, exclusive = true)]
    show_session: bool,

    // ... existing: src, since, until, k, model, min_score, json, no_escalate, debug_prompt, strict_citations ...
}
```

### 7.2 Behavior matrix

| Invocation | Behavior |
|---|---|
| `mur ask "Q"` | Default — archive prior session (if any), start fresh, single turn. |
| `mur ask --new "Q"` | Explicit alias for default. |
| `mur ask --continue "Q"` with prior session (≥1 turn) | Load last N turns, rewrite question, retrieve, generate, append turn. |
| `mur ask --continue "Q"` with no prior session | Error: `"no prior session; run without --continue to start a new one"`. Non-zero exit. |
| `mur ask --show-session` | No-LLM. Print summary. Ignore `question` if given. |
| `mur ask --continue --new "Q"` | Clap rejects (mutex). |

### 7.3 `--show-session` output

Plain text (not affected by `--json`):

```
session: /Users/david/.mur/conversations/ask-session.jsonl
turns: 3
last turn: 2026-04-21T15:42:00Z (4 minutes ago)
first question: "what did I ship this week?"
degraded turns: 0
```

Fields:
- `session` — absolute path.
- `turns` — count (0 if file missing or empty).
- `last turn` — timestamp of highest `turn_id` + human-readable delta.
- `first question` — `question` of `turn_id == 1`, truncated to 80 chars.
- `degraded turns` — count of turns with `degraded_to_mode_b == true` OR `rewriter_status == FailedFellBackToRaw`.

If session is empty: print `no active session. run 'mur ask "question"' to start one.` and exit 0.

### 7.4 JSON output integration

`AskResponse` gets two new fields for `--json` rendering:

```rust
pub struct AskResponse {
    // ... existing ...
    pub rewritten_question: Option<String>,    // same as TurnRecord.rewritten_question
    pub rewriter_status: RewriterStatus,       // same as TurnRecord.rewriter_status
}
```

Never breaks existing JSON consumers (all new fields are additive).

## 8. Config

One new field in `AskConfig`:

```rust
#[serde(default = "ask_default_continue_history_turns")]
pub continue_history_turns: u32,     // default 3
```

No `enabled` flag — the feature is gated by user flags (`--continue`), not config. No schema change.

No commander sync block (`sync_commander_config_toml` unchanged) — session state is ephemeral/local and doesn't feed the daemon.

## 9. Testing

### 9.1 Unit tests (Rust)

**`session::tests`** (`mur-core/src/conversations/ask/session.rs`):

- `load_latest_on_missing_file_returns_error` — cleanup state; `--continue` without a prior session errors cleanly.
- `load_latest_on_empty_file_returns_error` — 0-byte file treated like missing.
- `load_latest_skips_malformed_lines_with_warning` — insert a garbage line between two good lines; loader returns the two good ones.
- `load_latest_parses_valid_jsonl_in_order` — 3 lines → 3 turns, `turn_id` preserved.
- `append_turn_creates_file_if_missing` — first append creates file.
- `append_turn_appends_to_existing` — second append adds a line.
- `archive_and_new_renames_prior_to_history` — after archive, active file missing, `.history/*.jsonl` present.
- `archive_and_new_is_noop_on_empty` — no file → no history created.
- `archive_and_new_prunes_history_per_retain_config` — seed 7 archives with `retain=5` → 5 remain after prune.
- `last_n_returns_correct_slice` — 5-turn session, `last_n(3)` returns turns 3/4/5.

**`rewriter::tests`** (`mur-core/src/conversations/ask/rewriter.rs`):

- `empty_prior_turns_returns_identity_without_calling_ollama` — guard for turn 1 / no-continue case.
- `mock_mode_1_returns_identity` — `MUR_OLLAMA_MOCK=1` returns raw question, status is `NoRewriteNeeded`.
- `mock_mode_hash_returns_distinct_rewrite` — sets up a deterministic fixture where `hash`-mode produces a distinct rewrite (may require extending the hash-mode mock with a condense branch that shuffles text). Status is `Rewrote`.
- `connection_failure_returns_fallback_to_raw` — mock Ollama endpoint that errors; `FailedFellBackToRaw`.
- `response_identical_to_raw_is_classified_no_rewrite_needed` — whitespace/case-insensitive comparison.
- `truncates_prior_answers_to_500_chars` — prior turn with 2000-char answer renders 500-char truncated version into prompt.

**`prompt::tests`** (extend existing):

- `render_includes_chat_history_section_when_prior_turns_non_empty` — verify section markers + truncated answers.
- `render_drops_oldest_history_first_on_budget_overflow` — tight budget → oldest turn dropped, hits preserved.
- `render_falls_through_to_hit_shrinking_when_history_exhausted` — all history dropped, still over → hits shrink.

### 9.2 Integration tests (`mur-core/tests/cli_conversations.rs`)

- `mur_ask_continue_appends_to_session` — two invocations under `MUR_OLLAMA_MOCK=1`; assert `ask-session.jsonl` has 2 lines; assert second line's `rewriter_status` is a non-skipped variant.
- `mur_ask_new_archives_prior_session` — two `--new` invocations with content; assert `.history/` has 1 entry + active file has 1 turn.
- `mur_ask_show_session_prints_summary_without_ollama` — run `--show-session` WITHOUT `MUR_OLLAMA_MOCK`; assert success + expected substrings.
- `mur_ask_continue_without_prior_session_errors` — clean state + `--continue`; assert non-zero exit + error message substring.

### 9.3 Golden path (`scripts/golden-path-conversations.sh`)

Insert Steps 16 & 17 before the `ALL 15 STEPS GREEN` banner; update banner to `ALL 17 STEPS GREEN`.

```bash
# ── Step 16: mur ask --continue appends follow-up turn ──────────
echo "--- step 16: mur ask --continue (multi-turn) ---"
MUR_OLLAMA_MOCK=hash "$MUR" ask "what did I ship this week?" --json > /tmp/gp-step-16a.json
MUR_OLLAMA_MOCK=hash "$MUR" ask --continue "what about the prior week?" --json > /tmp/gp-step-16b.json
test -f "$TMPHOME/.mur/conversations/ask-session.jsonl" \
  || { echo "FAIL step 16: ask-session.jsonl missing"; exit 1; }
lines=$(wc -l < "$TMPHOME/.mur/conversations/ask-session.jsonl")
[ "$lines" -eq 2 ] \
  || { echo "FAIL step 16: expected 2 turns in session, got $lines"; exit 1; }
jq -e '.rewritten_question != null' <(tail -1 "$TMPHOME/.mur/conversations/ask-session.jsonl") \
  || { echo "FAIL step 16: second turn missing rewritten_question"; exit 1; }

# ── Step 17: mur ask --show-session prints summary ──────────────
echo "--- step 17: mur ask --show-session ---"
"$MUR" ask --show-session 2>&1 | tee /tmp/gp-step-17.txt
grep -q "turns: 2" /tmp/gp-step-17.txt \
  || { echo "FAIL step 17: show-session did not report turn count"; exit 1; }
grep -q "what did I ship this week" /tmp/gp-step-17.txt \
  || { echo "FAIL step 17: show-session did not echo first question"; exit 1; }
```

## 10. Success criteria

All of the following true at merge:

- `cargo test --workspace` green (existing + ~20 new tests).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo fmt --check` clean.
- `./scripts/golden-path-conversations.sh` prints `=== ALL 17 STEPS GREEN ===`.
- Phase 3.2 golden-path steps (11.5, 12, 13, 14) still pass — no regression.
- `mur ask --continue "Q"` with a prior session produces a second JSONL line whose `rewritten_question` is populated (either `Some(rewritten)` or `Some(raw)` depending on rewriter status) and whose retrieval used the rewritten query.
- `mur ask --show-session` prints a summary without invoking Ollama (can be run without `MUR_OLLAMA_MOCK`).
- `mur ask --continue` without a prior session produces a clear error message pointing to the solution.

## 11. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Rewriter adds 1-3s latency per follow-up turn | Deterministic-ish `temperature=0.1` + `num_predict=80` keeps it fast. Users can see it in `--json` output (`duration_ms` includes it). |
| Rewriter returns hallucinated question, degrading retrieval | LangChain prompt's "return as is" clause is a strong floor. `FailedFellBackToRaw` fallback preserves raw-question quality on LLM errors. |
| Session JSONL corrupts across crashes | `sync_all()` after every append + skip-malformed-line loader + never-fail-whole-load resilience. Crash durability matches Phase 2 writer pattern. |
| User expects `--continue` to work on their current-project (Claude Code mental model) but it's global | Document in `--continue` help text: "Continues the most recent session, global (not project-scoped). Use separate `MUR_HOME` roots for parallel threads." |
| `--show-session` confuses users when session is empty | Explicit friendly message: `no active session. run 'mur ask "question"' to start one.` |
| Hash-mode mock rewrite fixture requires mock engineering | Hash mode already produces deterministic text; adding a branch that prefixes "rewritten:" to mock-rewrite responses is a one-line change. |
| Budget math with history section may drop too aggressively | Tests `render_drops_oldest_history_first` and `render_falls_through_to_hit_shrinking` lock expected priorities. |

## 12. References

- Phase 3.2 design spec: `docs/superpowers/specs/2026-04-21-mur-conversations-phase-3-2-design.md`
- LangChain canonical condense prompt — widely used across LangChain/LlamaIndex/Haystack/Quivr.
- Chroma "Context Rot" research (2025) — smaller windows perform better for RAG accuracy.
- Mem0 chat-history summarization guide (Oct 2025) — last-N verbatim, summarize dropped.
- LangGraph persistence model — JSONL checkpoints per thread, append-only.
- Claude Code CLI local storage design — per-project JSONL with event-sourced session log.
- Gemini CLI session management — hashed-project JSONL, 30-day retention.
- OpenAI Agents SDK Sessions — SQLite-backed but event-log semantics match JSONL approach.
- MemGPT / A-MEM / Agentic Memory papers — agentic patterns explicitly deferred.
- HyDE, DMQR-RAG — upgrade-path references for Phase 3.4+.

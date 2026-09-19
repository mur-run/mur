# Turn ledger in multi-turn memory: the runtime remembers what a turn did, not what it said

**Status:** Drafted 2026-09-19 from a live incident (concierge `mur`, channel `01a0b304`, 2026-09-18 23:06Z). Design approved in conversation; three open decisions closed (§4.3 role, §4.2 hand-off, §3.2 excerpt scope). Not yet implemented.
**Scope:** `mur-agent-runtime/src/task_runner.rs` (`remember_turn`, `ConversationStore::remember`, `estimated_tokens`, `settle`, the `Action` record site), `mur-agent-runtime/src/turn_ledger.rs` (one projection type, one excerpt table), `mur-agent-runtime/src/llm/mod.rs` (one `RichMessage` variant), `llm/{anthropic,openai/mod,ollama}.rs` (render the variant), `llm/fallback/mod.rs` (skip it in `task_summary`). No wire-protocol change for callers; one Data part that was conditional becomes unconditional. No new YAML keys.
**Parent:** `turn_ledger.rs` module doc — the settlement ledger this spec re-uses. Related: `docs/superpowers/specs/2026-09-12-execution-limits-design.md` (where `TurnLedger` was introduced).
**Out of scope:** the second line of defence (flagging a completion claim made with zero tool calls) — §8.

## 1. Problem

On 2026-09-18 the concierge answered "這疊往 main 推一格" with a full report — #1402 merged, six branches rebased and force-pushed, GitHub bases updated — ten seconds after the message, from **one model call with zero tool calls**. Two turns later it quoted the "verbatim contents" of a file it had not read, and described a screenshot that was never attached. The user's complaint: "You never send any gh cmd! you just say but you do not do anything."

Telemetry (`~/.mur/agents/mur/telemetry/2026-09-18.jsonl`) rules out the usual suspects. Same model (`claude-opus-5`), same tool list (`tool_defs.clone()` on every call), same `intent: interactive`, agent idle when the message arrived. The only input that differed between a fabricating turn and a working turn was the conversation memory.

### 1.1 What the memory contains

`remember_turn` (`task_runner.rs:688`) is explicit:

```rust
/// Stores text only (this turn's tool scaffolding and any pasted image stay
/// ephemeral). Roles `user`/`agent` map to Anthropic `user`/`assistant`.
```

The persisted file for the fabricating turn (`conversations/01a0b6c4-….json`) is 24 pairs of `user: short imperative → agent: prose claiming completion`. No `tool_use`, no `tool_result`, no image. Two consequences:

- **The model cannot see what it actually ran.** It does not know that the previous read of `info.txt` returned `EDEADLK`; it only has its own sentence "009 卡在檔案上". "Read it again" therefore has no retry to make and no reason to make one.
- **Twenty-four in-context examples say "work looks like a report".** With no tool traffic anywhere in the window, the likeliest continuation of one more one-line command is one more report.

### 1.2 What already exists

The agentic loop already keeps a per-turn `TurnLedger` (`turn_ledger.rs`): one `Action { tool, target, outcome }` per tool call, recorded **at execution time** (`task_runner.rs:2375`, right after `apply_post_tool_use`), with failures structured as `Failed(<400 chars of the error>)`, `Denied(detail)`, `Running(job_id)`. `settle()` renders it as the settlement card and attaches it to the reply as a Data part — **but only when `warrants_settlement()`** (something changed, failed, is still running, or the turn stopped dirty). A turn that only read, and a turn that called nothing, produce identical memory: prose.

So the fix is not a new ledger. It is projecting the existing one into memory on every turn, including the empty one, and adding the three fields the incident showed were missing.

## 2. Goals and non-goals

Goals:

1. Every remembered turn carries a structured record of the tool calls it made, with failures verbatim (truncated) and targets intent-bearing (which path, which command).
2. A turn that called no tools is remembered as such, explicitly. That line is the counter-example the model needs.
3. A turn with no image attached is remembered as such, so "You have eyes" in the system prompt cannot outrank the record.
4. The record is not narrative: it lives in its own message variant, rendered under a fixed runtime-attributed header, never folded into assistant text.
5. Trimming never separates a ledger from its turn.

Non-goals:

- Replaying raw `ToolUse`/`ToolResults` into memory. Rejected: quarter-of-context budget is consumed in a few turns; tool results carry images; `call_id` pairing must hold across providers and across trims. The ledger is the compact form.
- Changing the user-visible settlement card or `warrants_settlement()`. The card is UX; this is memory.
- Judging whether the assistant's prose was honest. §8.

## 3. Data model

### 3.1 `TurnMemory` — the projection

In `turn_ledger.rs`, next to `TurnLedger`:

```rust
/// What one turn did, as remembered by the next turn. A projection of
/// `TurnLedger` plus the three facts memory needs and the card does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnMemory {
    /// How many images the user attached to this turn's input. `0` is the
    /// load-bearing value: "no image was attached" has to be on the record.
    pub attachments: u32,
    /// `tools.is_empty()`. Redundant on purpose — the rendered line
    /// `narrative_only: true` is the counter-example the model reads.
    pub narrative_only: bool,
    pub tools: Vec<ToolMemory>,
    /// Calls beyond `MEMORY_ROWS` were dropped from `tools`; this is the count.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolMemory {
    pub tool: String,
    /// `describe_target()` — the command, path, or fleet. Already the
    /// intent-bearing argument; not re-parsed into a structured `args`.
    pub target: String,
    pub status: ToolMemoryStatus,          // ok | failed | denied | running
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,             // failed/denied: the detail, ≤ RUNAWAY_BACKSTOP
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,           // ok only, §3.2; absent when nothing matched
}
```

`TurnMemory::from(&TurnLedger, attachments)` is the only constructor. `Action` gains one optional field, `excerpt: Option<String>`, filled at the record site (§4.1) — `TurnLedger`'s serialized shape grows by one skippable field, which the Hub/TUI consumers of the settlement card do not read.

Row cap: `MEMORY_ROWS = 25`. Text caps: `error` and `excerpt` both ≤ `RUNAWAY_BACKSTOP` (400), the module's existing single length limit. Both are constants in `turn_ledger.rs`.

### 3.2 Excerpt table

An excerpt exists only for `bash` calls with `Outcome::Ok`, and only when the command's first token(s) match one row. Anything else: no excerpt. Absent beats noise.

| command prefix | line kept |
|---|---|
| `cargo test`, `cargo nextest` | the last line starting with `test result:` |
| `gh pr view`, `gh pr list` | lines starting with `state` or `mergeable` (`--json` output or table form), joined with `, ` |
| `git status` | the first non-empty line |

The table is a `const` slice of `(prefix, extractor)` in `turn_ledger.rs`, beside `is_evidence()`'s gate-command list. It is deliberately three rows; growing it is a one-line change with a test each. Matching is on the command string after `describe_target()` (which is the `command` argument), with one leading `cd <path> && ` stripped the same way `is_evidence()` already strips it — and nothing more: `is_evidence()`'s doc explains why shell composition beyond that prefix is deliberately not understood, and the same reasoning holds here.

### 3.3 `RichMessage::TurnLedger`

```rust
pub enum RichMessage {
    Text { role, content },
    ToolUse { text, calls },
    ToolResults { results },
    ImageText { role, media_type, data, text },
    /// The runtime's record of what the preceding assistant turn did.
    /// Stored in multi-turn memory only; never produced by a provider.
    TurnLedger { turn: u32, memory: crate::turn_ledger::TurnMemory },
}
```

`turn` is the value of `turn_counter` for that turn — the same number the hook `TurnCtx` carries, so a ledger can be matched to telemetry.

## 4. Data flow

### 4.1 Produced in the loop (unchanged site, one more field)

At `task_runner.rs:2375` the `Action` is built from `call` and `entry`; `excerpt` is computed there from `entry.content` because that is the only place the result text is in hand. Nothing else in the loop changes.

### 4.2 Handed to `remember_turn` through the reply

`settle()` today attaches `application/vnd.mur.turn-ledger+json` only when `warrants_settlement()`. It now attaches it **on every turn**. Grepped 2026-09-19: no consumer of that MIME type exists in `mur-core`, `mur-hub-gui`, or the mobile SDK, and neither murmur nor the Hub renders unknown Data parts, so the change is invisible to clients and hands the Hub a free per-turn record.

`remember_turn` gains the ledger by reading that part back out of `reply`. Turns that never enter the agentic loop — the single-call path at `task_runner.rs:1721` (`tools: vec![]`), `RunnerBackend::StubEcho` / `StubSlow` / `Misconfigured`, the CLI-spawn backends — carry no Data part, and `remember_turn` synthesizes `TurnMemory { attachments, narrative_only: true, tools: [] }` for them. The synthesis is the default, so a future backend that forgets the part still lands the counter-example rather than nothing.

`attachments` is counted in `remember_turn` from `input.parts`: Data parts whose `mime_type` starts with `image/` — the same predicate `user_message()` uses to build `ImageText`, extracted into one helper so the two cannot drift.

Rejected alternative: widen `run_agentic_loop`'s return to `(Message, Option<LoopExit>, TurnLedger)`. It threads a third value through every early-exit path (`graceful_exit`, stuck, withdrawn, deadline) for no benefit the Data part does not give, and it leaves the Hub without the record.

### 4.3 Stored shape

A remembered turn is a triple, in this order:

```
Text{user} | ImageText{user}   ← the input (image data is still not stored: ImageText is downgraded to Text)
Text{agent}                    ← text_of(reply), as today (settlement card included when present)
TurnLedger{turn, memory}       ← new
```

`ImageText` is downgraded to `Text` on store exactly as today; the fact that an image was attached survives in `attachments`.

### 4.4 Rendered to the provider

All three HTTP adapters render `TurnLedger` as **one user-role text message**:

```
<turn_ledger turn="42" source="runtime">
attachments: 0
narrative_only: false
tools:
  - tool: read_file
    target: /Users/david/Documents/murmur-issues/009/info.txt
    status: failed
    error: "Resource deadlock avoided (os error 11)"
  - tool: bash
    target: gh pr view 1402 --json state
    status: ok
    excerpt: "state: MERGED"
</turn_ledger>
```

The YAML body is produced by one function, `render_memory(&TurnMemory) -> String`, in `turn_ledger.rs`; adapters wrap it, they do not format it. The header is a constant.

Why user role: the ledger is something the runtime observed, not something the model said. Putting it in the assistant turn makes it part of the narrative that caused the problem; putting it in a user turn makes it testimony the model reads about itself. Anthropic's Messages API merges consecutive same-role turns; the OpenAI-compatible and Ollama chat formats accept them as-is. No adapter needs a merge step.

`fallback::task_summary` (first user text in the request, for the routing log) skips the variant. `fallback::requirements_of` is untouched: a ledger carries no image and asks for no tool.

### 4.5 Budget and trimming

`estimated_tokens` counts `render_memory(...).len()` for the variant, so the ledger competes for the same quarter-of-context budget as everything else.

`ConversationStore::remember` currently drains the oldest **two** messages per step. It now drains the oldest **turn**: everything from index 0 up to (not including) the next user-role `Text`/`ImageText`. This is what keeps a ledger and its assistant text together, and keeps the history starting on a user turn — the `MAX_CONV_MESSAGES` invariant comment ("MUST stay even") is replaced by "MUST start on a user turn", which the new drain guarantees by construction. `MAX_CONV_MESSAGES` itself becomes a soft ceiling applied by whole turns.

Old conversation files are not migrated. A file without `TurnLedger` entries deserializes as before (serde adds a variant; it does not remove one). Its turns simply have no ledger, which is honest: the runtime did not record one.

## 5. Error handling

- The Data part is malformed or missing → synthesize `narrative_only` (§4.2). Memory never fails a turn.
- `render_memory` output exceeds what the budget allows for a single turn → the trim loop drops the oldest turn, never truncates a ledger; if the newest turn alone exceeds the budget, it is stored anyway (same as today for an oversized reply).
- A provider rejects consecutive user messages → not expected for the three adapters; if a future adapter needs it, it merges in its own `to_messages`, the same place it already flattens `ToolUse` text.

## 6. Testing

`turn_ledger.rs`:

- `excerpt` table: one positive and one negative case per row; a `cargo test` with no `test result:` line yields `None`.
- `TurnMemory::from`: `narrative_only` true iff no tools; `more` counts overflow past `MEMORY_ROWS`; `error` populated for `Failed` and `Denied`, absent for `Ok`.
- `render_memory`: the header constant, `attachments: 0` printed (not skipped) for zero.

`task_runner.rs`:

- `remember_turn` stores the triple in order; the ledger comes from the Data part when present.
- Single-call and echo paths store a synthesized `narrative_only: true` ledger.
- Trimming removes whole turns, never leaves a ledger without its agent text, and the surviving history starts on a user turn. Rewrite `history_is_trimmed_by_tokens_not_turn_count` for the triple.
- Attachments counted from `image/*` Data parts; a text-only input records `0`.
- Regression lock for the incident: seed a history of 24 text-only pairs plus one turn with an empty ledger, call `seed_history`, and assert the rendered message stream contains `narrative_only: true` immediately before the new user message. This locks the mechanism, not model behaviour — the spec does not claim the model will now call tools, only that it can now see whether it did.

Adapters (`anthropic`, `openai`, `ollama`): one test each — a `TurnLedger` renders as a user-role message whose text starts with the header constant and contains the YAML body verbatim.

`fallback`: `task_summary` returns the real user text when a `TurnLedger` precedes it.

## 7. Rollout

One PR, one crate. The unconditional Data part is the only externally observable change and has no observers. No config, no env var, no migration. Verify on the live concierge by reading `~/.mur/agents/mur/conversations/<latest>.json` after one tool-using turn and one pure chat turn: the first ends in a ledger with rows, the second in `narrative_only: true`.

## 8. Not in this spec: the second line of defence

Flagging a reply that claims completion while its ledger is empty is a separate design. The hard part is defining "claims completion" without also catching a pure question ("what does X do?") answered correctly with no tools. Direction agreed in conversation: mark and annotate when the event lands on the channel, never refuse. The ledger this spec adds is the input that design needs; it is filed as the follow-on.

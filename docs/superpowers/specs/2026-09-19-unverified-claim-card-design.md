# Unverified-claim settlement card: a zero-tool turn that names external state says so

**Status:** Drafted 2026-09-19 from the same incident as `2026-09-19-turn-ledger-memory-design.md` (§8 there filed this as the follow-on). Design approved in conversation with three amendments (§2.1 diff signals and boundary rules, §3 wording, §4 ledger flag). Not yet implemented.
**Scope:** `mur-agent-runtime/src/turn_ledger.rs` (one predicate, one `TurnLedger` field, one `render` branch, one `warrants_settlement` clause) and `mur-agent-runtime/src/task_runner.rs` (`settle` evaluates the predicate). No change to `mur-core`, the TUI, the channel schema, or memory.
**Parent:** `docs/superpowers/specs/2026-09-19-turn-ledger-memory-design.md` — the first line of defence (the model can now see whether it ran anything). This is the second: the user can too.
**Out of scope:** refusing, rewriting or retrying a reply; detecting quoted file contents ("內容逐字如下"); any model-based classifier.

## 1. Problem

After the first line of defence, a turn that called no tools is remembered as `narrative_only: true` — the *next* turn's model sees it. The *user* still sees only prose. On 2026-09-18 the prose was "推了一格：#1402 已 merged，`main` 現在是 `1e0a4d40`，六個 rebase 完成" and nothing distinguished it, on screen or in the channel, from a turn that had run `gh pr merge`.

The settlement card (`turn_ledger::render`) already exists for exactly this job: a table the runtime assembles from its own records, appended to the reply text so every consumer sees it, upgraded to a card by the TUI. It is withheld from a turn that ran nothing because `warrants_settlement()` treats "nothing happened" as "nothing to say" — correct for a pure question, wrong for a report.

The gap is therefore one predicate: which zero-tool replies are reports.

## 2. The gate

### 2.1 Signals

A zero-tool reply earns the card when its text carries **external-state evidence**: strings that are usually the *product of a tool* (git, gh, a test runner), not of natural language. The list is deliberately structural — no completion vocabulary in any language, so a rephrased report does not slip through and a well-worded answer is not caught. Mis-fires are accepted (§2.3).

| Signal | Rule | Usually produced by |
|---|---|---|
| git object id | a maximal run of `[0-9a-fA-F]` of length 7–40, all lowercase hex, containing at least one letter | git / gh |
| PR / issue ref | `#` followed by 2+ digits, where the character before `#` is not a word character (`[A-Za-z0-9_]`) or is start-of-text | GitHub |
| GitHub state word | `\b(?:MERGED\|MERGEABLE\|CONFLICTING\|UNSTABLE)\b` (all caps, the API's own strings) | `gh … --json` |
| test tally | `\d+ passed`, `\d+ failed`, or `test result:` | a test runner |
| GitHub URL | `github\.com/\S+/(?:pull\|commit\|issues)/` | gh / browser |
| diff / patch | a line starting with `diff --git`, `@@`, `+++ ` or `--- ` | git diff |
| diff stat | `\d+ files? changed`, `\d+ insertions?\(\+\)`, `\d+ deletions?\(-\)` | git |

**Boundaries without lookaround.** The runtime's regex crate (`regex = "1"`) has no lookbehind/lookahead, and `\b` is unreliable next to CJK text (`main現在是1e0a4d40` must match). So the first two rows are not regexes: a hand scan finds maximal hex runs (a run is bounded by any non-hex character, which is what `(?<![0-9a-fA-F])…(?![0-9a-fA-F])` would express) and checks the `#` rule by looking at the preceding character. The remaining rows use `regex` with the patterns shown; `\b` is fine there because the tokens are ASCII words.

`claims_external_state(text: &str) -> bool` lives in `turn_ledger.rs` beside `is_evidence`, with the table as constants. Cost is one pass over the reply.

### 2.2 The clause

```rust
pub fn warrants_settlement(&self) -> bool {
    !self.changed().is_empty()
        || !self.blocked().is_empty()
        || !self.running().is_empty()
        || !self.stop.is_clean()
        || self.unverified_claim()      // new
}

/// Ran nothing, yet the reply names external state.
pub fn unverified_claim(&self) -> bool {
    self.actions.is_empty() && self.claims_external_state
}
```

`claims_external_state` is a `TurnLedger` field (§4), set by `settle()`.

### 2.3 Accepted mis-fires and misses

- **Mis-fire:** the user pastes a SHA or `#1402` and asks what it is; the model answers from memory with no tool. The card appears. The card's sentence ("no tool ran this turn") is still true, and the cost is one muted line.
- **Miss:** "info.txt 內容逐字如下：…" — a fabricated file quotation carries none of the shapes above. Catching it means a vocabulary table, which is the thing this design refuses. Recorded here so it is not re-proposed as a bug.
- **Miss:** an all-digit 7+ run (`1234567`) is not a SHA — no letter. Accepted; `#` and the other rows usually co-occur in a real report.

## 3. What the card says

No new UI. `render()` gains one branch: when `ledger.unverified_claim()`, the verified row is

```
─ settlement ─
  ⚠ unverified  no tool ran this turn — external state claims above were not checked
```

instead of the existing `⚠ verified   nothing ran — no evidence this works`. Same `⚠` glyph, so the TUI paints it `settlement_muted` (`settlement.rs::row_styles`): a warning about missing evidence, not a failure. No other row applies (nothing changed, nothing blocked, nothing running).

Because the card is part of the reply text, it reaches murmur, `--plain`, `mur agent send`, the Hub, and the channel event with no further plumbing — the same property the settlement card was built for.

## 4. Structured mark

`actions: []` on the ledger Data part cannot tell a pure question from an unverified report, so the ledger carries the gate's result:

```rust
pub struct TurnLedger {
    …
    /// The reply text carried external-state evidence (§2.1). Set by `settle`;
    /// `unverified_claim()` is this AND no actions. Additive: absent on
    /// ledgers written before it existed.
    #[serde(default, skip_serializing_if = "is_false")]   // fn is_false(b: &bool) -> bool { !*b }
    pub claims_external_state: bool,
}
```

A consumer computes `unverified = claims_external_state && actions.is_empty()`. `TurnMemory` (the model-facing projection) is unchanged: the model already gets `narrative_only`, and the card text sits in its own stored reply.

`settle()` becomes:

```rust
fn settle(text: String, ledger: &TurnLedger) -> Message {
    let mut ledger = ledger.clone();
    ledger.claims_external_state = claims_external_state(&text);
    // … existing body, using `ledger` …
}
```

The two call sites (`task_runner.rs` end-turn and `graceful_exit`) are untouched.

## 5. Error handling and non-effects

- The predicate cannot fail; a reply of any size is scanned once.
- Nothing is refused, rewritten, retried, or re-prompted. `stop`, `iterations`, token counts, HITL state: unchanged.
- Memory (`remember_turn`) is unchanged; the card is in the stored reply text as today.
- A turn that ran tools is never affected: `unverified_claim()` requires `actions.is_empty()`, and `warrants_settlement()`'s other clauses decide as before.

## 6. Testing

`turn_ledger.rs`:

- `claims_external_state` positives, one per row: `1e0a4d40`; `#1402`; `the PR is MERGED now`; `3048 passed`; `https://github.com/mur-run/mur/pull/1402`; a `diff --git a/x b/x` line; `2 files changed, 10 insertions(+)`.
- Boundary: `main現在是1e0a4d40` matches (CJK adjacency); `1234567` does not (no letter); `#1` does not (one digit); `abc#1402` does not (`#` preceded by a word char); `deadbeef00` inside a longer hex run `1deadbeef00ff…` of 41+ chars does not (run too long).
- Negatives: `你好`; a paragraph explaining how rebase works with no ids; `Version 2.85.0`.
- `warrants_settlement`: empty ledger + claim → true; empty ledger, no claim → false (the existing "a pure question earns no card" test stays); one `read_file` action + claim → false (tools ran; the other clauses decide).
- `render`: empty ledger + claim prints the `⚠ unverified` row and not `nothing ran`; empty ledger, no claim prints nothing (no card).
- Serde: a ledger JSON without the field deserialises with `false`; `true` round-trips; `false` is not serialised.

`task_runner.rs`:

- Regression lock: `settle("推了一格：#1402 已 merged，`main` 現在是 `1e0a4d40`。剩下六個全部 rebase 到新 main、force-push 完成。", &TurnLedger::default())` — the reply text contains `─ settlement ─` and `unverified`, and the Data part has `claims_external_state: true`.
- Negative control: `settle("哈囉，今天想折騰點什麼？", &TurnLedger::default())` — no card, no field.
- Existing `the_turn_before_a_new_message_says_whether_it_ran_anything` feeds the incident text through `settle` with an empty ledger and asserts only on the stored `TurnLedger` entry; it keeps passing, now with the card inside the stored reply text.

## 7. Rollout

One PR, one crate, no config. Live check: in murmur, ask the concierge a question whose answer it can only narrate (e.g. "把 #1402 的狀態列出來，不要用工具") and confirm the muted `⚠ unverified` row under the reply; then ask "你好" and confirm no card.

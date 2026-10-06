# Agent review loop Phase 2 — implementation plan

> **Execution:** `mur-executing-plans` (sequential, in-context), or `mur-delegate-dev` if a
> coding fleet member is running. Every task follows `mur-tdd`: failing test first.
> Track progress by ticking the checkboxes in this file.

- **Goal:** turn escalation from a Stop into a wait for a human `/rule`, and give `/rule` real
  fold semantics, per `docs/superpowers/specs/2026-10-05-agent-review-loop-phase2-design.md`
  (cited below as `P2-§x`; the Phase 1 spec as `P1-§x`).
- **Architecture:** the ledger fold gains ruling state and one query, `pending_ruling()`. A new
  module `ruling.rs` owns `/rule` parsing and the single ruling step (`settle_rulings`) that both
  the live loop and `review-resume` call, so there is one prompt and one set of outcomes. The
  terminal prompt lives behind `ReviewTransport`, so tests script it.
- **Tech stack:** Rust 2024, `mur-core` crate, module `mur-core/src/cmd/fleet/review/`. Base:
  `main` @ `400b1354`.

## Global constraints (every task)

- `ledger.pending_ruling()` is the **only** definition of "a ruling is owed" (P2-§4, R2).
- The driver never writes an `escalation` event (R3).
- `q` at the ruling prompt never writes `session_stopped`; only `/abandon` and the kill-switch end
  the session from that prompt (R8).
- Replay equals the live ledger (P1 AC11, P2 AC-P2-10). Any in-memory ledger change must come from
  `Ledger::apply` on the same payload that is appended.
- No hardcoded strings or numbers outside `constants.rs` (CLAUDE.md rule 2).
- Source files ≤ 800 lines (CLAUDE.md rule 5). Current sizes: `ledger.rs` 572, `session.rs` 499,
  `schema.rs` 450, `loop_driver.rs` 433, `resume.rs` 275.
- Lint gate after every task:
  `cargo clippy --all --all-targets --no-deps --locked -- -D warnings && cargo fmt --all -- --check`

## Decisions D1–D3 (human, 2026-10-05)

Three places where the spec was ambiguous or wrong against `main`. All three are decided and the
spec is already amended (Draft rev 4); the plan below implements the amended text.

| # | Problem found in `main` | Decision (spec section) |
|---|---|---|
| D1 | The rev 3 spec queued a `/rule` typed during a turn until "the next turn boundary". The reviewer's turn boundary is mid-round (main's rebuttal is folded in `round_ledger` but written only at the seal), so live and replay would apply the ruling at different points (AC-P2-10). And the stdin driver reads nothing during a turn. | `/rule` outside the ruling prompt is read **only at a send prompt**; the line is send consent **and** a ruling. Applied only at a round boundary: at main's prompt at once (ruling written, main's message rebuilt, sent without re-asking); at the reviewer's prompt held until after the seal. A held ruling is **re-validated against the open set (`open` ∪ `disputed`)**: still there → written; `resolved`/`withdrawn` → discarded with a notice. (P2-§5.3, AC-P2-8, AC-P2-18) |
| D2 | P1's crashed path writes `paused{crashed}` + `resumed` (`resume.rs:231`), contrary to the rev 3 crashed column; and a `paused` written after the prompt would make `active_time` count the human wait. | Resume writes `paused { kind: other, reason: "crashed" }` **after the lock, before the prompt**. One column for paused and crashed. (P2-§6, AC-P2-5, AC-P2-17) |
| D3 | Nothing writes `HumanNote` on `main`; the "repeated-note prompt" R7 and §9 relied on does not exist. | Phase 2 scope is formally reduced: plain-text notes, `@<agent>`, `@<unknown>`, and the repeated-note prompt are out of scope (Phase 3). (P2-§0, R7, §9) |

## PR slicing

Three PRs, cut at the Task 5 / Task 6 line:

| PR | Content | Behaviour change |
|---|---|---|
| 1 | Phase 2 spec (rev 4) + this plan | none (docs) |
| 2 | Tasks 1–5: schema, fold, replay property, `/rule` parsing, post-`fix` reject restriction | none — the fold gains fields and `pending_ruling()`, but nothing in the driver calls them yet |
| 3 | Tasks 6–11: transport hooks, live loop, session end, binding notes, resume, docs | yes |

PR 2's items with no non-test caller (`pending_ruling()`, `binding_rulings`, `ruling.rs` parse
entry points) carry `#[allow(dead_code)] // wired in PR 3 (Task 6–7)`; PR 3 removes every one of
those attributes (`git grep 'wired in PR 3' -- '*.rs'` must be empty before it merges). Open PR 3 right
after PR 2 merges.

## File structure

| File | Change | Responsibility |
|---|---|---|
| `review/schema.rs` | modify | `Ruling` reshape, `RulingDecision`, `PauseKind`, `Paused.kind`, remove `Escalation` |
| `review/ledger.rs` | modify | fold rules 1–4, `EscalationRecord.handled`, `Finding.ruled`, `pending_ruling()`, `binding_rulings()` |
| `review/ledger_tests.rs` | **new** | the existing `#[cfg(test)] mod tests` moved out of `ledger.rs` (pure move, first commit of Task 2) plus Phase 2 fold tests |
| `review/ledger_replay_tests.rs` | modify | AC-P2-10 property test |
| `review/ruling.rs` | **new** | `/rule` line parsing, `RulingAnswer`, `settle_rulings` (live + resume) |
| `review/ruling_tests.rs` | **new** | parser and `settle_rulings` tests |
| `review/verdict.rs` | modify | `parse_rebuttal` refuses `reject` on a `fix`-ruled finding |
| `review/driver.rs` | modify | `ReviewTransport::ask_ruling`, `confirm_send → SendAnswer`, `TurnOutcome::{RuleFirst, Sent{held}}`; `write_paused_and_revert` sets `kind: Transport` |
| `review/session.rs` | modify | `TerminalGate` implements the ruling prompt and `/rule` at send prompts; resume branching |
| `review/loop_driver.rs` | modify | call `settle_rulings` after a `revise` seal; main-prompt ruling + rebuild; apply held rulings after the seal; reset `last_activity` |
| `review/wire.rs` | modify | inject binding rulings into both turn prompts |
| `review/constants.rs` | modify | prompt strings, hints, `REVIEW_STOP_REASON_ESCALATION` |
| `review/resume.rs` | modify | crashed pre-prompt `paused`; `resumed` written by caller choice |
| `review/state.rs` | modify | drop the `Escalation` arm |
| `docs/superpowers/specs/2026-10-04-agent-review-loop-design.md` | modify | P2-§7 edits |
| `docs/superpowers/specs/2026-10-05-agent-review-loop-phase2-design.md` | — | already amended to rev 4 (D1–D3); `mur verify` only |

---

## Task 1 — Schema

**Interfaces.** Consumes: nothing. Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RulingDecision { Drop, Fix }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseKind { Escalation, Transport, Detached, User, #[default] Other }

// in ReviewPayload
Ruling { finding: String, decision: RulingDecision, text: String },
Paused {
    #[serde(default)] kind: PauseKind,
    reason: String,
    #[serde(flatten)] cumulative: Cumulative,
},
// ReviewPayload::Escalation removed.
```

- [ ] Test `schema.rs`: `phase1_paused_without_kind_parses_as_other` — deserialize
  `{"v":1,"type":"paused","reason":"x","exec_time_ms":0,"cost_usd_micros":0}` (copy the exact tag
  shape from an existing `Paused` round-trip test) → `kind == PauseKind::Other` (AC-P2-11).
- [ ] Test `ruling_round_trips` — serialize/deserialize `Ruling { finding: "F1", decision: Fix,
  text: "use the cache" }`, assert equal.
- [ ] Test `escalation_payload_no_longer_parses` — an envelope with `"type":"escalation"` →
  `classify_note_payload` returns `NoteClassification::Malformed` (damage, P1-§8.2).
- [ ] Run, watch all three fail.
- [ ] Implement the types; delete `Escalation`. Fix every constructor the compiler flags:
  `driver.rs:267` (`kind: PauseKind::Transport`), `resume.rs:232` (`kind: PauseKind::Other`),
  `ledger.rs:199-211` (temporary: `Ruling { .. } => {}`, Task 2 replaces it), `state.rs:95` (drop
  the arm), test fixtures in `driver_tests.rs`, `ledger_replay_tests.rs`, `prune_tests.rs`,
  `state_tests.rs`, `resume_tests.rs` (`kind: PauseKind::Other`).
- [ ] Remove `#[allow(dead_code)]` on `FindingStatus::is_closed` only if Task 2 uses it; otherwise
  leave.
- [ ] Run the review module tests green, lint gate, commit `feat(review): phase 2 schema`.

## Task 2 — Ledger fold

**Interfaces.** Consumes: Task 1 types. Produces:

```rust
pub struct EscalationRecord { pub finding_id: String, pub reason: String, pub handled: bool }
pub struct Finding { /* existing */ pub ruled: Option<RulingDecision> }
pub struct RulingRecord { pub finding: String, pub decision: RulingDecision, pub text: String }

impl Ledger {
    /// P2-§4 "Single definition".
    pub fn pending_ruling(&self) -> Vec<&EscalationRecord>;
    /// Rulings not yet delivered to `role` (cleared when a `turn_sent { to: role }` folds).
    pub fn binding_rulings(&self, role: Role) -> &[RulingRecord];
}

pub enum FoldError {
    /* existing */
    RulingForUnissuedFinding(String),
    ReopenAfterDrop(String),
    DisputedAfterFix(String),
}
```

`binding_rulings` state: `Ledger.unseen_rulings: [Vec<RulingRecord>; 2]` indexed by role; a
`Ruling` pushes to both, `TurnSent { to }` clears that role's list. Derived purely from events, so
replay matches (Global constraint).

- [ ] **Pure move first:** move `ledger.rs`'s `#[cfg(test)] mod tests` into `ledger_tests.rs`
  (`#[cfg(test)] #[path = "ledger_tests.rs"] mod ledger_tests;`, same pattern as `resume.rs`).
  No behaviour change. Tests green. Commit `refactor(review): move ledger tests out`.
- [ ] Failing tests in `ledger_tests.rs`:
  - `ruling_drop_resolves_and_handles_escalation` — issue F1, reject twice (escalation), apply
    `Ruling{F1, Drop}` → `F1.status == Resolved`, `ruled == Some(Drop)`, `pending_ruling()` empty,
    `escalations.len() == 1` with `handled == true`.
  - `ruling_fix_reopens_and_resets_rejects` — F1 disputed + escalated, `Ruling{F1, Fix}` → status
    `Open`, `reject_count == 0`, `pending_ruling()` empty (AC-P2-4 fold half).
  - `reject_after_fix_is_ignored_by_fold` — after Fix, a rebuttal `reject` on F1 leaves
    `reject_count == 0` and adds no escalation (P2-§4 rule 1).
  - `reviewer_reopening_dropped_finding_is_damage` — after Drop, `FindingStatus{F1, Open}` →
    `Err(ReopenAfterDrop("F1"))`; `FindingStatus{F1, Resolved}` → `Ok` (rule 3, AC-P2-3).
  - `disputed_after_fix_is_damage` — after Fix, `FindingStatus{F1, Disputed}` →
    `Err(DisputedAfterFix("F1"))`; `Open` and `Resolved` → `Ok` (rule 4).
  - `ruling_on_unissued_finding_is_damage` → `Err(RulingForUnissuedFinding("F9"))`.
  - `proactive_fix_without_escalation_folds_like_ac4` — no escalation, `Ruling{F1, Fix}` → same
    finding state as the escalated case, `escalations` empty (AC-P2-9).
  - `second_ruling_wins` — `Fix` then `Drop` on F1 → `ruled == Some(Drop)`, `Resolved`.
  - `binding_rulings_clear_per_role_on_turn_sent` — after a ruling both roles see it; after
    `TurnSent{to: Main}` only the reviewer still does.
- [ ] Run, watch them fail.
- [ ] Implement in `Ledger::apply`:
  - `Rebuttal` reject: `if f.ruled == Some(RulingDecision::Fix) { continue; }` before the count.
  - `Ruling { finding, decision, text }`: find or `Err(RulingForUnissuedFinding)`; mark every
    `escalations` entry with that `finding_id` and `!handled` as handled; `reject_count = 0`;
    `ruled = Some(*decision)`; `status = Resolved` for Drop, `Open` for Fix; push `RulingRecord`
    to both `unseen_rulings`.
  - `FindingStatus`: if `ruled == Some(Drop)` and new status ≠ `Resolved` → `ReopenAfterDrop`;
    if `ruled == Some(Fix)` and new status == `Disputed` → `DisputedAfterFix`.
  - `TurnSent { to, .. }`: clear `unseen_rulings[to]`.
  - New escalations push `handled: false`.
- [ ] Add the three new variants to `rollback.rs::fold_error_reason` (the compiler forces it; give
  each a reason string from `constants.rs`) so they surface as damage like the P1 variants.
- [ ] Green, lint, commit `feat(review): fold rulings (P2-§4)`.

## Task 3 — Replay property (AC-P2-10)

**Interfaces.** Consumes: Task 2. Produces: nothing new.

- [ ] Extend the P1 AC11 property test in `ledger_replay_tests.rs`: its event generator gains
  `Ruling` events on issued IDs (both decisions) and `TurnSent` events; assert
  `fold_rounds(events) == live ledger built by apply` including `ruled`, `handled`,
  `unseen_rulings`.
- [ ] Add the fixed case from D1: sealed round, then `Ruling`, then the next round's
  `TurnSent{Main}`; and a round whose `Ruling` precedes its own `TurnSent{Main}` (main-prompt
  ruling); assert a staged-then-dropped trailing round still keeps the ruling
  (P2-§2 "A ruling therefore survives the drop of an unsealed trailing round").
- [ ] Fail → fix (only if needed) → green → commit `test(review): replay across rulings`.

## Task 4 — `/rule` parsing (`ruling.rs`, pure)

**Interfaces.** Consumes: Task 1, Task 2. Produces:

```rust
pub struct RulingInput { pub finding: String, pub decision: RulingDecision, pub text: String }

pub enum PromptLine { Rule(RulingInput), Abandon, Leave, Other(String /* hint */) }

/// One line read at the ruling prompt. `raw.is_empty()` = EOF → `Leave` (P2-§5.2).
/// `open` = the ledger's open-set IDs; a ruling on anything else is `Other(hint)` (P2-§5.4).
pub fn classify_ruling_line(raw: &str, open: &BTreeSet<String>) -> PromptLine;

/// `/rule drop|fix F<n> <text>` → Ok, else Err(hint). Used by the send prompt too.
pub fn parse_rule_command(line: &str, open: &BTreeSet<String>) -> Result<RulingInput, String>;
```

Constants in `constants.rs`: `RULE_COMMAND = "/rule"`, `ABANDON_COMMAND = "/abandon"`,
`LEAVE_PAUSED_KEY = "q"`, `RULE_USAGE_HINT = "usage: /rule drop|fix F<n> <text>"`,
`RULE_NOT_OPEN_HINT = "{id} is not an open finding"`.

- [ ] Tests in `ruling_tests.rs`: `""` → `Leave`; `"q\n"` → `Leave`; `"Q"` → `Leave`;
  `"/abandon"` → `Abandon`; `"/rule drop F1 dup of F2"` with F1 open → `Rule{F1, Drop, "dup of F2"}`;
  `"/rule fix F1"` (no text) → `Other(usage)`; `"/rule keep F1 x"` → `Other(usage)`;
  `"/rule drop F7 x"` with F7 not open → `Other(not-open hint naming F7)`; `"hello"` → `Other`;
  `"\n"` (Enter) → `Other` (Enter is **not** leave — only `q` and EOF are).
- [ ] Fail → implement → green → lint → commit `feat(review): /rule parsing`.

## Task 5 — Rebuttal restriction after `fix` (P2-§5.3)

**Interfaces.** Consumes: Task 2 (`Finding.ruled`). Produces: no new names.

- [ ] Test in `verdict.rs` tests: ledger with F1 `ruled == Some(Fix)`; rebuttal answering F1
  `reject` → `Err` whose text contains `REVIEW_FIX_RULED_REJECT_HINT` with `F1`; `partial` with a
  reason → `Ok`; `accept` → `Ok`.
- [ ] Add constant `REVIEW_FIX_RULED_REJECT_HINT = "finding {id} was ruled `fix` by the human; answer accept or partial, not reject"`.
- [ ] In `parse_rebuttal`, after the reason check, return that error. The existing
  retry-once → `Blocked{Main}` path then gives AC-P2-4's driver half for free.
- [ ] Green → lint → commit.

## Task 6 — Transport hooks and the terminal prompt

**Implements D1.** **Interfaces.** Consumes: Task 4. Produces:

```rust
// driver.rs
pub enum SendAnswer { Send, Stop, SendWithRuling(RulingInput) }

// ReviewTransport
/// P2-§5.1 step 3: show `pending` and read one line. Default (tests, non-terminal): EOF.
fn ask_ruling(&self, pending: &EscalationRecord, ledger: &Ledger) -> Result<String> { Ok(String::new()) }
/// P2-§5.3 send prompt. Replaces P1's `-> Result<bool>`; the 4 existing impls
/// (`session.rs`, `driver_tests.rs`, `guards.rs`, `session_tests.rs`) map true/false to
/// Send/Stop. `open` is the open set the `/rule` line is validated against.
fn confirm_send(&self, member: &str, params: &Value, open: &BTreeSet<String>) -> Result<SendAnswer> { Ok(SendAnswer::Send) }

// run_turn / run_turn_with_retry gain `boundary: bool` (true only for main's turn) and
// `pre_confirmed: bool` (skip `confirm_send`; used for the rebuilt main send).
pub enum TurnOutcome {
    Stopped,
    /// Sent; `held` is a ruling typed at a non-boundary (reviewer) prompt.
    Sent { reply: String, held: Option<RulingInput> },
    /// `/rule` at a boundary (main) prompt: nothing sent; the loop applies it and re-sends.
    RuleFirst(RulingInput),
}
```

`RetryOutcome` mirrors the two new shapes. On a transport retry `confirm_send` is **not** asked
again for the retry (the first answer stands, including any `held` ruling).

`ask_ruling` returns the **raw line**, not a parsed answer, so the kill-switch check in Task 7
runs before parsing (P2-§5.1 step 4).

- [x] Tests in `session_tests.rs` with an injected reader (refactor `TerminalGate` to hold
  `input: &dyn Fn() -> io::Result<String>`, defaulting to stdin; the pure refactor is its own
  commit):
  - `ask_ruling_prints_both_positions_and_prompt` — output contains the finding issue, main's last
    rebuttal reason, and `RULING_PROMPT` with `{id}` substituted.
  - `ask_ruling_time_counts_as_human_wait` — `take_human_wait()` > 0 after a delayed reader.
  - `send_prompt_rule_is_send_with_ruling` — reader yields `"/rule drop F1 x\n"` →
    `SendAnswer::SendWithRuling` with F1/drop/`x`; the reader is called once (no second prompt).
  - `send_prompt_rejects_rule_on_closed_finding` — F1 not in `open` → hint printed, re-prompted;
    the next `"\n"` gives `Send`.
  - `send_prompt_p1_answers_unchanged` — `"\n"`/`y`/`yes` → `Send`; `q`, other text, EOF →
    `Stop`.
  - `run_turn_boundary_rule_sends_nothing` / `run_turn_reviewer_rule_is_held` (`driver_tests.rs`)
    — scripted transport; send count 0 and `RuleFirst`, resp. send count 1 and
    `Sent { held: Some(_) }`.
- [x] Constants: `RULING_PROMPT = "Awaiting ruling on {id} — /rule drop|fix {id} <text>, /abandon, q = leave paused "`,
  and extend the send prompt text to `[Enter = send, q = stop, /rule … = record a ruling]`.
- [x] "Both sides' last positions" needs main's last reason for the finding: add
  `Finding.last_reject_reason: Option<String>` set in the `Rebuttal` fold (derived, replay-safe)
  and a ledger test for it.
- [x] Fail → implement → green → lint → commit.

## Task 7 — `settle_rulings` and the live loop

**Interfaces.** Consumes: Tasks 2, 4, 6. Produces:

```rust
pub enum RulingOutcome { Settled, LeftPaused, Abandoned, KillSwitch }

pub struct RulingCtx<'a> {
    pub transport: &'a dyn ReviewTransport,
    pub mur_home: &'a Path,
    pub fleet_name: &'a str,
    pub channel_id: &'a str,
    /// true at the `review-resume` prompt of a session already paused (P2-§6, AC-P2-17).
    pub already_paused: bool,
}

/// P2-§5.1 steps 3–5 and P2-§5.2, for every pending escalation in order.
/// Appends `ruling` (and applies it to `ledger`) or `paused { kind: Escalation }`; never
/// `session_stopped` — the caller maps Abandoned/KillSwitch to a LoopDriverStop.
pub fn settle_rulings(ctx: &RulingCtx, ledger: &mut Ledger, cumulative: Cumulative) -> Result<RulingOutcome>;
```

Algorithm, per `pending_ruling()[0]` until empty:
1. `line = transport.ask_ruling(..)`
2. `if control::is_stopped(mur_home, fleet_name) { return Ok(KillSwitch) }` — before parsing,
   nothing written.
3. `match classify_ruling_line(&line, &open_ids)`:
   `Rule(r)` → build `ReviewPayload::Ruling`, `ledger.apply` then append (same payload), loop;
   `Abandon` → `Ok(Abandoned)`; `Leave` → if `!already_paused` append
   `Paused { kind: Escalation, reason: REVIEW_PAUSE_REASON_ESCALATION, cumulative }` and apply it;
   `Ok(LeftPaused)`; `Other(hint)` → print hint, re-ask.
4. Empty → `Ok(Settled)`.

Loop changes (`loop_driver.rs`):
- Replace the `Revise if !ledger.escalations.is_empty()` arm with
  `Revise if !ledger.pending_ruling().is_empty()` → `settle_rulings(..., cumulative from
  self.elapsed())`; then `self.human_wait += transport.take_human_wait()`; map
  `Settled` → `self.last_activity = now()` and fall through to the round-stuck check;
  `LeftPaused` → `LoopDriverStop::Paused { reason }`; `Abandoned` → `LoopDriverStop::Escalation`;
  `KillSwitch` → `LoopDriverStop::Stopped`.
- Main's turn (`boundary = true`): on `RuleFirst(r)` → `control::is_stopped` first (→ `Stopped`,
  nothing written); then `apply_ruling(ctx, ledger, r)` (append + fold, reset `last_activity`);
  rebuild `main_turn_params` from the new ledger; print `RULING_REGENERATED_BANNER`
  (`"[ruling applied; message regenerated]"`) and then the full rebuilt message (always, no
  diffing — P2-§5.3); send with `pre_confirmed = true`.
  Several `/rule` lines are impossible here: the line was the send consent.
- Reviewer's turn (`boundary = false`): push `held` into `self.held: Vec<RulingInput>`.
- After the seal, in the `Revise` arms **before** the `pending_ruling()` check:
  `apply_held_rulings(ctx, ledger, &mut self.held)` — for each, `ledger.open_set()` contains the
  ID → append + fold, reset `last_activity`; else print `RULING_DISCARDED_CLOSED_NOTICE`
  (`"Ruling on {id} discarded: finding is already {status}."`) and write nothing. On `Approve` /
  `ReviewerBlocked` the held rulings are dropped with `RULING_DISCARDED_SESSION_END_NOTICE`
  (`"Ruling on {id} discarded: session ended with {verdict}."`, `verdict` ∈ `approve|blocked`).
- `LoopDriverStop::Escalation` doc: "the human typed `/abandon` at the ruling prompt";
  `stop_reason` → `REVIEW_STOP_REASON_ESCALATION` (`"escalation"`, unchanged wire value).
- Keep `loop_driver.rs` ≤ 800 lines; `apply_ruling` and `apply_held_rulings` live in `ruling.rs`.

- [x] Tests (scripted transport in `loop_driver_tests/`, new file `rulings.rs`):
  - AC-P2-1 `escalation_waits_for_ruling` — scripted ask returns EOF; assert no
    `session_stopped`, no `escalation` type on the channel, `paused{kind: escalation}` last.
    (Fleet-definition and lock parts are asserted in Task 8 via `run_session`.)
  - AC-P2-2 `drop_ruling_then_revise_does_not_stop` — ask returns `/rule drop F1 x`; the next
    `revise` round runs; the loop ends on a later `approve`.
  - AC-P2-4 driver half `reject_after_fix_blocks_main` — `/rule fix F1 x`; main then rejects F1
    twice → `Blocked { role: Main }`.
  - AC-P2-7 `ruling_wait_trips_neither_limit` — fake clock advances 15 min inside `ask_ruling`
    with `stuck = 10m`, `deadline = 5m`; loop continues; next `turn_sent.human_wait_ms ≥ 15 min`.
  - AC-P2-8a `main_prompt_rule_applies_before_send` — main's confirm returns
    `SendWithRuling(fix F1)` in round 2; channel order `ruling`, round-2 `turn_sent{main}`; the
    text main received contains `REVIEW_BINDING_RULINGS_HEADER`; confirm asked once that turn;
    the transport's printed output has `RULING_REGENERATED_BANNER` followed by exactly the text
    main received.
  - AC-P2-8b `reviewer_prompt_rule_lands_after_seal` — reviewer's confirm returns
    `SendWithRuling` in round 1; reviewer still sent; order: round-1 `verdict`, `ruling`,
    round-2 `turn_sent{main}`.
  - AC-P2-18 `held_rule_revalidated` — three cases where the round-1 reviewer verdict moves F1
    to `resolved`, `withdrawn`, `disputed`: the first two → no `ruling` on the channel,
    `RULING_DISCARDED_CLOSED_NOTICE` printed with that status, round 2 runs; `disputed` → `ruling` written before round-2 `turn_sent{main}` and
    F1 folded (`fix` → `open`).
  - AC-P2-19 `held_rule_dies_with_session` — reviewer's confirm returns `SendWithRuling` in a
    round whose verdict is (a) `approve`, (b) `blocked`: loop result as without the ruling, no
    `ruling` on the channel, `RULING_DISCARDED_SESSION_END_NOTICE` printed and
    `RULING_DISCARDED_CLOSED_NOTICE` not printed.
  - `main_prompt_rule_kill_switch` — `.stopped` created inside main's confirm → `Stopped`, no
    `ruling`, nothing sent.
  - AC-P2-12 `q_leaves_paused` — ask returns `q`; `LoopDriverStop::Paused`; no `session_stopped`.
  - AC-P2-13 `abandon_stops` — `/abandon` → `LoopDriverStop::Escalation`.
  - AC-P2-14 `kill_switch_discards_input` — `.stopped` created inside `ask_ruling`, which returns
    `/rule drop F1 x`; result `Stopped`; no `ruling` event on the channel.
  - AC-P2-15 `eof_is_q` — same assertions as AC-P2-12 with `""`.
  - `invalid_rule_reprompts_without_writing` — `/rule drop F9 x` then `q`; one `paused`, zero
    `ruling`.
  - Update the existing P1 test `flow.rs:337` (escalation stops the loop) to the new behaviour.
- [x] Fail → implement → green → lint → commit `feat(review): wait for a ruling on escalation`.

## Task 8 — Session end and stop screen

**Interfaces.** Consumes: Task 7. Produces: no new names.

- [x] Tests (`session_tests.rs`):
  - AC-P2-1 remainder: `run_session` with EOF at the ruling prompt → fleet definition still
    present, channel has no `session_stopped`.
  - AC-P2-13 remainder: `/abandon` → `session_stopped { reason: "escalation" }`, fleet definition
    removed, `prepare_resume` refuses (`has ended` — the missing definition is checked before
    `session_stopped`).
  - `stop_screen_for_escalation_pause` — `render_stop_screen(Paused{..})` prints the
    `review-resume` hint (it already does for any `Paused`; assert it, no code change expected).
- [x] `end_session` needs no change (Paused → keep fleet; others → stop). Green, lint, commit.

## Task 9 — Binding notes in turn prompts (P2-§5.3)

**Interfaces.** Consumes: Task 2 `binding_rulings(role)`. Produces: no new names.

- [x] Tests (`wire.rs` tests): after a ruling, `main_turn_params` and `reviewer_turn_params` text
  both contain `REVIEW_BINDING_RULINGS_HEADER` and the ruling text, and the block appears **before**
  the open-findings list; with no rulings the header is absent.
- [x] Add `{binding_rulings}` to `REVIEW_MAIN_PROMPT` and `REVIEW_REVIEWER_PROMPT`, placed above
  `{open_findings}`; render `"- {id} {decision}: {text}"` per record; substitute **before** `{task}`
  and `{main_reply}` (the existing placeholder-injection rule in `wire.rs`).
- [x] Green, lint, commit.

## Task 10 — Resume (P2-§6)

**Implements D2.** **Interfaces.** Consumes: Tasks 7, 8. Produces:

```rust
// resume.rs
pub enum Lifecycle { WriteResumed, None }
/// `resume_session` no longer writes `paused{crashed}`; `prepare_resume` does, right after the
/// lock (D2), so `Resumable.crashed` stays for the banner only.
pub fn resume_session(transport, mur_home, r: Resumable, retry_delay) -> Result<(Ledger, LoopDriverStop)>;
```

`cmd_fleet_review_resume` branches on the ledger, in this order:
1. `!r.ledger.pending_ruling().is_empty()` → `settle_rulings(ctx { already_paused: true }, ..)`
   (true for crashed too, after D2's pre-prompt `paused`). `Settled` → `resume_session`;
   `LeftPaused` → print `Left paused.`, return; `Abandoned` → `append_session_stopped("escalation")`
   + `remove_session_fleet` + stop screen; `KillSwitch` → same as the P1 kill-switch stop.
2. last review event is `Ruling` with no later `Resumed` → prompt `RULING_RECORDED_CONTINUE_PROMPT`
   (`"Ruling recorded — continue? [Enter = continue, q = leave paused] "`).
3. otherwise the unchanged P1 paused/crashed prompt.

- [x] Tests (`resume_tests.rs`):
  - AC-P2-5 `sigkill_while_awaiting_resumes_at_ruling_prompt` — channel ends at a sealed `revise`
    round with a pending escalation, lock free, no `paused` → resume asks the ruling prompt, not
    the crashed continue prompt; a `paused{crashed}` precedes any ruling.
  - AC-P2-6 `paused_escalation_rule_resumed_next_round` — order: `paused{escalation}`, `ruling`,
    `resumed`, next round's `turn_sent{main}`.
  - AC-P2-16 `abandon_from_resume` — both paused and crashed fixtures → `session_stopped{escalation}`,
    fleet definition gone.
  - AC-P2-17 `q_on_paused_resume_writes_nothing` — paused fixture: channel tail byte-identical
    before/after; crashed fixture: the only new event is the pre-prompt `paused{crashed}`; lock
    released in both (a second `prepare_resume` succeeds).
  - `kill_switch_at_resume_prompt` — `.stopped` set during the ask → P1 kill-switch stop, no
    `ruling`.
  - `active_time_excludes_ruling_wait_on_crashed` — D2 regression: 10 min sleep in the scripted
    ask does not raise `active`.
  - Update the P1 crashed test (`resume_tests.rs:307`) to expect `paused{crashed}` written by
    `prepare_resume`.
- [x] Fail → implement → green → lint → commit `feat(review): resume at the ruling prompt`.

## Task 11 — Specs and docs

- [ ] Apply the P2-§7 table to `docs/superpowers/specs/2026-10-04-agent-review-loop-design.md`.
- [ ] Phase 2 spec: already amended to rev 4 before implementation (D1–D3). Re-check that the
  constants' final texts match the prompts quoted in P2-§5.1 and §5.3.
- [ ] P1 spec §6: mark the plain-text / `@<agent>` / `@<unknown>` / repeated-note rows *not
  built; Phase 3* (P2-§7 row for §6).
- [ ] User-facing docs: run the `update-docs` skill for README, docs site, product page (new
  ruling prompt, `/rule`, `/abandon`).
- [ ] `mur verify --file` on both specs.

## Self-review

- **Spec coverage:** P2-§3.1–3.3 → T1; §3.4, §4 → T2 (+T6 `last_reject_reason`); AC-P2-10 → T3;
  §5.1 steps 3–5, §5.2 → T4, T6, T7; step 1 (force semi-auto) → no code, Phase 2 never observes
  auto (spec says so); §5.1 human-wait + stuck reset → T7 AC-P2-7; §5.3 → T5, T6 (send prompt), T7 (apply + re-validate), T9;
  §5.4 → T4, T6; §6 → T10; §7 → T11. AC-P2-1…19 each name a test above; AC-P2-3 → T2, AC-P2-9 → T2,
  AC-P2-11 → T1.
- **Placeholders:** none; every hint and prompt is a named constant with its text.
- **Type consistency:** `RulingDecision`, `PauseKind`, `RulingInput`, `PromptLine`,
  `RulingOutcome`, `RulingCtx`, `pending_ruling()`, `binding_rulings(role)`,
  `settle_rulings`, `apply_ruling`, `apply_held_rulings`, `SendAnswer` are spelled identically in every task that uses them.

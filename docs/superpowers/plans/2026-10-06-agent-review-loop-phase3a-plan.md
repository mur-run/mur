# Agent review loop Phase 3a — implementation plan

> **Execution:** `mur-executing-plans` (sequential, in-context), or `mur-delegate-dev` if a
> coding fleet member is running. Every task follows `mur-tdd`: failing test first.
> Track progress by ticking the checkboxes in this file.

- **Goal:** let the human send notes to the agents from the stdin send prompt (`/note <text>`,
  `@<agent> <text>`), per `docs/superpowers/specs/2026-10-06-agent-review-loop-phase3a-design.md`
  (cited as `P3a-§x`; earlier specs as `P1-§x` / `P2-§x`).
- **Architecture:** the session parses the line and returns `SendAnswer::Note`. The driver owns
  an in-memory pending queue, rebuilds the message from `(ledger, pending)` before every prompt
  through one generator, and flushes the queue to the channel in an `on_consented` callback
  that runs after the gate says `Go` and before `transport.send`. The ledger fold gains
  `unseen_notes`, cleared by `TurnSent` like `unseen_rulings`.
- **Tech stack:** Rust 2024, `mur-core`, module `mur-core/src/cmd/fleet/review/`. Base: `main` @
  `92056060`.

## Global constraints (every task)

- Replay equals the live ledger (P1 AC11, AC-P3a-14). Every in-memory ledger change comes from
  `Ledger::apply` on the same payload that is appended.
- **`ReviewPayload::HumanNote` wire bytes do not change.** Phase 1/2 channels and new ones must
  serialize and parse identically (`{"type":"human_note","text":…}` plus `"target"` only when
  `Some`). Task 1 pins this with a byte-exact test; it is the base that AC-P3a-14 stands on.
- The reprinted message and the sent message come from one generator call (N5). No code path
  builds turn params by hand.
- `/note` and `@<agent>` are never send consent (N2). Only Enter, `y`, `/rule` are.
- No hardcoded strings or numbers outside `constants.rs` (CLAUDE.md rule 2).
- Source files ≤ 800 lines (CLAUDE.md rule 5). Current sizes: `loop_driver.rs` 615,
  `session.rs` 587, `schema.rs` 529, `ledger.rs` 445, `driver.rs` 396, `wire.rs` 355. Note
  parsing goes in a new `note.rs`, not `session.rs`.
- Test strings asserted with `contains` are distinctive tokens (`NOTE-A`, `RULING-TXT`), never
  a single letter or common word: a fixed prompt or header containing `Y` makes
  `contains("Y")` pass or fail for the wrong reason (found in Task 6).
- Lint gate after every task:
  `cargo clippy --all --all-targets --no-deps --locked -- -D warnings && cargo fmt --all -- --check`

## Decisions D1–D2 (human, 2026-10-06)

| # | Problem found against `main` | Decision |
|---|---|---|
| D1 | P3a-§5.2 rev 3 said a boundary `/rule` rebuilds and **prompts again**; P2-§5.3 (`…phase2-design.md:226`) says "sent without asking again — the `/rule` line was the send consent", pinned by `mur-core/src/cmd/fleet/review/loop_driver_tests/rulings.rs` line 437 (`confirm:main` == 2). P3a's own N2 sides with P2. | Keep P2. After `RuleFirst` the rebuilt message (pending notes included) is printed in full and sent with no prompt; the notes are flushed before that send. Channel order: `ruling`, `human_note…`, `turn_sent`. P3a spec amended to **rev 4** in this PR (§4, §5.2, §5.3 retry, AC-P3a-9, AC-P3a-14). P2 and its tests do not change. |
| D2 | P3a-§2 calls `HumanNote { text, target }` a type; on `main` it is only a `ReviewPayload` variant (`schema.rs:284-289`), so `SendAnswer::Note(HumanNote)` has nothing to name. | Add `pub struct HumanNote { text: String, target: Option<Role> }` in `schema.rs` with a conversion to `ReviewPayload::HumanNote`. The variant keeps its inline fields; wire bytes byte-identical (global constraint). |

## The two-ledger rule (key correctness point)

`HumanNote` has no round, so `fold_rounds` applies it to **both** the sealed ledger and the open
attempt's scratch (`ledger.rs:429-434`). The live loop must do the same, or a session that stops
mid-round returns a ledger replay cannot reproduce (the mirror of Phase 2 PR 3's `turn_sent`
trap).

- A note flushed at **main's** prompt (first send or resend): `ledger.apply(note)`. `round_ledger`
  does not exist yet; it is cloned from `ledger` later, so it inherits the note.
- A note flushed at the **reviewer's** prompt (first send or resend): `ledger.apply(note)` **and**
  `round_ledger.apply(note)`. `ledger` is what `Turn::Stop` returns mid-reviewer-turn
  (`loop_driver.rs:384`); `round_ledger` is what the verdict seal adopts.
- Tested in Task 6 (AC-P3a-14 extended case) by
  `reviewer_side_note_then_stop_replays_equal_to_live` in `mur-core/src/cmd/fleet/review/loop_driver_tests/notes.rs`:
  reviewer-side note, then the send fails twice → paused; `fold_rounds(channel) == live ledger`.

## PR slicing

| PR | Content | Behaviour change |
|---|---|---|
| 1 | This plan + P3a spec rev 4 (D1) | none (docs) |
| 2 | Tasks 1–4: `HumanNote` struct, fold, replay property, `note.rs` parsing, note rendering | none — nothing calls the new parser or passes notes yet |
| 3 | Tasks 5–9: generator + `on_consented` in the driver, loop wiring, session prompt, docs | yes |

PR 2 items with no non-test caller carry `#[allow(dead_code)] // wired in PR 3 (Task 5–7)`; PR 3
removes every one (`git grep 'wired in PR 3' -- '*.rs'` must be empty before it merges).

## File structure

| File | Change | Responsibility |
|---|---|---|
| `mur-core/src/cmd/fleet/review/schema.rs` | modify | `HumanNote` struct, `From<HumanNote> for ReviewPayload` (no separate `into_payload`; `From` was enough) |
| `mur-core/src/cmd/fleet/review/ledger.rs` | modify | `unseen_notes: [Vec<HumanNote>; 2]`, fold arms, `unseen_notes(role)` |
| `mur-core/src/cmd/fleet/review/ledger_tests.rs` | modify | fold tests |
| `mur-core/src/cmd/fleet/review/ledger_replay_tests.rs` | modify | property: notes interleaved with turns, live == replay |
| `mur-core/src/cmd/fleet/review/note.rs` | **new** | `/note`, `@<agent>`, unknown `/<word>` line parsing; alias map; member resolution |
| `mur-core/src/cmd/fleet/review/note_tests.rs` | **new** | parser tests (AC-P3a-4, 5, 15, 16, 17) |
| `mur-core/src/cmd/fleet/review/wire.rs` | modify | `main_turn_params` / `reviewer_turn_params` take `pending: &[HumanNote]`; `{human_notes}` slot |
| `mur-core/src/cmd/fleet/review/constants.rs` | modify | `REVIEW_HUMAN_NOTES_HEADER`, hints, `SEND_PROMPT` text, flush-failure pause reason |
| `mur-core/src/cmd/fleet/review/driver.rs` | modify | `SendAnswer::Note`, generator + `on_consented` in `run_turn_with_retry` |
| `mur-core/src/cmd/fleet/review/loop_driver.rs` | modify | pending queue, generator per side, two-ledger flush, `RuleFirst` via generator |
| `mur-core/src/cmd/fleet/review/session.rs` | modify | `TerminalGate` gains `members: [String; 2]` + `mur_home`; dispatches to `note.rs` |
| `mur-core/src/cmd/fleet/review/driver_tests.rs`, `mur-core/src/cmd/fleet/review/session_tests.rs`; `mur-core/src/cmd/fleet/review/loop_driver_tests/notes.rs` (**new**) | modify/new | AC tests |
| `docs/superpowers/specs/2026-10-06-agent-review-loop-phase3a-design.md` | modify | rev 4 (D1) — done in PR 1 |
| README, docs site, product page | modify | `update-docs` skill, Task 9 |

---

## Task 1 — `HumanNote` struct (D2)

**Produces:**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanNote {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Role>,
}
impl From<HumanNote> for ReviewPayload { /* ReviewPayload::HumanNote { text, target } */ }
```

- [x] Test `human_note_wire_bytes_unchanged`: serialize `ReviewPayload::HumanNote` built via
  `From<HumanNote>` for `target: None` and `Some(Role::Reviewer)`; assert the JSON **string**
  equals the literal bytes produced on `main` today (capture them first from the existing
  variant, paste as literals — `{"type":"human_note","text":"x"}` and
  `{"type":"human_note","text":"x","target":"reviewer"}`).
- [x] Test `phase1_human_note_parses`: the two literals deserialize to the expected variant.
- [x] Watch fail (struct missing), implement, green, lint, commit `feat(review): HumanNote struct`.

## Task 2 — Ledger fold (P3a-§6.1)

- [x] Tests in `ledger_tests.rs`: broadcast note → both slots; `Some(Main)` → main slot only;
  `TurnSent { to: Main }` clears main slot, reviewer slot kept; note never touches findings or
  round; `Ruling` + note in either order keep independent queues.
- [x] Implement `unseen_notes` next to `unseen_rulings` (`ledger.rs:86`), replace the no-op arm
  `ledger.rs:289`, add the clear beside `ledger.rs:192`, accessor `unseen_notes(role) -> &[HumanNote]`.
- [x] `ledger_replay_tests.rs`: extend the generator so notes (both targets) appear between
  turns, including between a main `turn_sent` and its verdict; assert live fold == `fold_rounds`.
- [x] Green, lint, commit `feat(review): fold human notes`.

## Task 3 — `note.rs` parsing (P3a-§3, N3, N10, N11)

**Produces:**

```rust
pub enum NoteLine { Note(HumanNote), Hint(String), NotNote }
pub fn parse_note_line(line: &str, members: &[String; 2], resolve: impl Fn(&str) -> String) -> NoteLine;
```

`resolve` is `canonicalize_agent_name(mur_home, _)` in production, a closure in tests. Order:
`/rule` is not handled here (`NotNote`); `/note` → note or `usage: /note <text>`; `@` → alias map
(`@主` → Main, `@審查` → Reviewer, constants, checked first), else `resolve` then **must equal**
`members[0]` or `members[1]`, else N3 hint; `@`/`@x` with no text → `usage: @<agent> <text>`;
any other `/<word>` → `unknown command: /<word>`; everything else → `NotNote`.

- [x] Tests (`note_tests.rs`): AC-P3a-15 empty `/note`; AC-P3a-4 alias path and name path as
  separate tests (name path with a `resolve` that changes case); AC-P3a-5 `@nobody`; AC-P3a-17
  `resolve` returns a real but non-member name → N3 hint; AC-P3a-16 `/foo` and `/riule drop F1 x`;
  `q`, `nope`, `""` → `NotNote`; `/rule …` → `NotNote`.
- [x] Implement; strings in `constants.rs`. `#[allow(dead_code)] // wired in PR 3 (Task 5–7)`.
- [x] Green, lint, commit `feat(review): note line parser`.

## Task 4 — Note rendering (P3a-§6.2)

- [x] Tests in `wire` tests: for `to`, block = `ledger.unseen_notes(to)` then pending with target
  `None` or `to`, in that order; other-side-only pending notes absent; empty → no block and the
  message is byte-identical to today's; block appears before the findings section (and after the
  binding-rulings block); AC-P3a-18 — neither template contains a justify-non-adoption
  instruction.
- [x] Golden test `no_notes_message_is_byte_identical_to_before`: with no unseen and no pending
  notes (and with other-side-only pending notes), both messages, with and without binding rulings,
  equal byte-for-byte the text `main` rendered before the slot existed
  (`review/testdata/wire_golden/*.txt`, captured from `d60f20a7`). Same guarantee as Task 1's wire
  bytes, at the message instead of the channel. Mutation-checked: an extra `\n` from an empty
  block fails it; the older `no_rulings_no_header` does not catch that.
- [x] Golden maintenance. The files are versioned and frozen: a red golden test means the
  no-note path changed the message, so fix the template, not the file. Regenerate only for an
  intentional template or constant change:
  `MUR_BLESS_WIRE_GOLDEN=1 cargo test -p mur-core --lib -- --ignored bless_wire_golden`
  (an ignored test, a no-op without the env var), then review the `.txt` diff in the PR. The same
  rule is in the doc comment on `GOLDEN` in `wire.rs`.
  `mur-core/src/cmd/fleet/review/testdata/wire_golden/.gitattributes` sets `* -text` so Windows `autocrlf` checkouts keep the
  LF bytes (CI on `windows-latest` failed on CRLF before it).
- [x] Add `{human_notes}` slot to both templates in `constants.rs`, `REVIEW_HUMAN_NOTES_HEADER`,
  extend `main_turn_params(task, round, ledger, pending)` and
  `reviewer_turn_params(task, round, main_reply, ledger, pending)`; existing callers pass `&[]`.
- [x] Green, lint, commit `feat(review): render human notes`. **End of PR 2.**

## Task 5 — Driver: generator and `on_consented` (P3a-§4, §5.3)

**Changes in `driver.rs`:**

```rust
pub enum SendAnswer { Send, Stop, SendWithRuling(RulingInput), Note(HumanNote) }

pub fn run_turn_with_retry(
    transport, mur_home, fleet_name, member,
    build: &dyn Fn(&[HumanNote]) -> serde_json::Value,   // ledger captured by the caller
    pending: &mut Vec<HumanNote>,
    on_consented: &mut dyn FnMut(&[HumanNote]) -> Result<()>,  // appends + applies; Err = pause
    g: SendGate, channel_id, retry_delay,
) -> Result<RetryOutcome>
```

- The gate loop: `params = build(pending)`; `confirm_send` → `Note(n)` pushes and loops;
  `Stop` returns (pending left for caller to discard); `RuleFirst` returns (pending kept);
  `Go` → `on_consented(pending)`, then `pending.clear()`, then send with **the same `params`**.
- `pre_confirmed: true` skips `confirm_send` but still runs `on_consented` (D1: the send after
  `RuleFirst` flushes). The inner transport retry reuses `params` and never calls it again.
- Flush failure: the callback returns `Err(FlushFailed { recorded: k-1, total: n, cause })`; the
  driver writes `paused { kind: other, reason }` with the constant
  `note flush failed after <k-1> of <n> notes; …` (P3a-§5.3), prints it, returns `Paused`.
- `run_turn` (single attempt) keeps taking `&params`; it is the inner step.
- [ ] Tests (`driver_tests.rs`, `RuleGate` extended per P3a-§8 test-double note: scripted answers
  incl. `Note`, records every `params` shown): `Note` then `Send` → callback saw one note, sent
  params == last shown params; `Note` then `Stop` → callback never called (AC-P3a-7 driver half);
  AC-P3a-12 first send fails, retry ok → callback called once; AC-P3a-13 callback fails at k=2 of 2
  → paused reason says 1 of 2, no send; `pre_confirmed` → callback called, no prompt.
- [ ] Implement, green, lint, commit `feat(review): note queue in the send gate`.

## Task 6 — Loop wiring (P3a-§5, two-ledger rule)

- [x] `LoopRun::turn` takes `build: impl Fn(&Ledger, &[HumanNote]) -> Value` and
  `validate: impl Fn(&Ledger, &str) -> Result<T, String>` (validate gets the ledger as an
  argument so it no longer borrows `round_ledger` while the flush mutates it). Resend prompt =
  `build(...)` + hint, every attempt.
- [x] Pending queue lives on `LoopRun` (one per turn; cleared on `Stop`, kept across `RuleFirst`).
- [x] Flush callback appends each note in order via `self.append`, then applies it per the
  two-ledger rule. Main side: `ledger`. Reviewer side: `ledger` and `round_ledger`.
- [x] `RuleFirst` path (`loop_driver.rs:318-333`): keep the kill-switch check, `write_ruling`,
  banner + full reprint; replace the hand-written `main_turn_params` with the generator; set
  `pre_confirmed = true`; no prompt (D1).
- [x] Tests in new `mur-core/src/cmd/fleet/review/loop_driver_tests/notes.rs`: AC-P3a-1, 2, 3, 6, 7, 8 (rev 5: note before `turn_sent` and in the message; ruling held — absent before that `turn_sent` and from the message, lands after the round's verdict and before main's next `turn_sent`; mutation: writing held rulings before `turn_sent` fails it), 9 (assert channel order
  `ruling`, `human_note(A)`, `turn_sent`; sent bytes == reprinted; `confirm:main` count equals the
  P2 test's), 10, 11 (+ `review-resume` path shows A again), 14 for 1/3/9/10/11 **and** the
  reviewer-side stop case from the two-ledger rule.
- [x] Two-ledger tests, named (implementation without these is not done):
  - `reviewer_side_note_then_stop_replays_equal_to_live`: note flushed at the reviewer's prompt,
    send fails twice → `Turn::Stop` (`loop_driver.rs:384`) returns `ledger`; assert
    `fold_rounds(channel) == ` that ledger, and the note is in it. Mutation check: drop the
    `ledger.apply` on the reviewer side → fails.
  - `reviewer_side_note_survives_verdict_seal`: note flushed at the reviewer's prompt, verdict
    accepted; the sealed ledger (adopted from `round_ledger`) equals replay. Mutation check: drop
    the `round_ledger.apply` → fails.
- [x] Unchanged: `mur-core/src/cmd/fleet/review/loop_driver_tests/rulings.rs` line 437 still asserts 2. Do not edit it.
- [x] Green, lint, check `loop_driver.rs` ≤ 800 (689 lines; the planned turn-helper split was
  not needed), commit
  `feat(review): pending notes in the review loop`.

## Task 7 — Session prompt (P3a-§3, N4, N10, N11)

- [x] `TerminalGate` gains `members: [String; 2]` and `mur_home: &Path`, set at
  `session.rs:422` and `:491` from `fleet.members`. Test constructors updated.
- [x] `confirm_send`: valid/invalid `/rule` unchanged first; then `parse_note_line` →
  `Note` returns `SendAnswer::Note`, `Hint` prints and re-asks, `NotNote` falls through to the
  existing Send/Stop logic.
- [x] `SEND_PROMPT` mentions `/note` and `@<agent>`. There was no pinned test on `main`; added
  `send_prompt_names_note_and_at_agent`.
- [x] Found in Task 7: on a case-insensitive filesystem (default macOS APFS)
  `canonicalize_agent_name` hits its exact-match branch for `@Reviewer` and returns the name as
  typed, so the N11 "equals a member" check failed for a real member. `note.rs::member_role` now
  compares exactly first, then ASCII case-insensitively — the resolver's own rule. Membership is
  still required (AC-P3a-17 unchanged); pinned by
  `member_name_as_typed_still_matches_on_case_insensitive_disks` and
  `send_prompt_member_name_resolves_through_mur_home`.
- [x] Tests (`session_tests.rs`): each row of P3a-§3 through a scripted `TerminalGate`;
  `send_prompt_p1_answers_unchanged` passes **unchanged**; AC-P3a-16 `/riule` re-asks.
- [x] Remove every `wired in PR 3` attribute. Green, lint, commit `feat(review): /note and @agent at the send prompt`.
- [x] Follow-up: the case-insensitive fallback in `member_role` resolves only when exactly one member folds to the typed name; two members differing only in case are refused (N3 hint) instead of resolved by list order. Test: `case_only_member_collision_is_deterministic`.

## Task 8 — End-to-end check

- [x] Full review-module test run plus `cargo nextest run -p mur-core review` (see `docs/BUILD.md`).
- [x] `git grep 'wired in PR 3' -- '*.rs'` empty.
- [x] Result: `nextest -p mur-core review` 503/503. Full `nextest -p mur-core`: 8398 run, 8389 passed, 9 failed, none in `review::`. 8 are the pre-existing `serena_install::with_fake_uv` failures (4 tests, lib + bin), red on `main` too; 1 is `agent_start_without_symlink`, an environment denial (`spawn $TMPDIR/.../mur-agent-runtime: Operation not permitted`) from running under the MUR agent seal; `main` (d8bf20f4) fails the same test with the identical error in the same sandbox. No new failures.

## Task 9 — Docs

- [x] `mur verify --file` on the P3a spec (11/11) and this plan (33/33 after fixing four stale
  paths: two short-form test paths, a planned `into_payload` method → the `From` impl that was built,
  and the planned turn-helper split that was not needed at 689 lines).
- [x] Spec rev 6: N11 and §3 add the exact-then-ASCII-case-insensitive member match; AC-P3a-4
  names it. Task 7's pinned-test line already reads "no pinned test on `main`; added
  `send_prompt_names_note_and_at_agent`".
- [x] README: `/note` and `@<agent>` in the fleet review bullet. `mur verify` on README reports
  one stale claim, the fleet review subcommand at L630; it is the same on the unmodified README, because
  the installed `mur` predates the subcommand. Not from this PR.
- [x] `mur-server` branch `docs/fleet-review-notes`: a `## Notes` section on the `fleet-review`
  page (prompt grammar, lifecycle, hints with the exact strings from `constants.rs`) and one
  sentence on the product card. Tutorials do not mention the send prompt; untouched. Open that
  PR only after a release carries PR 3; it deploys on merge and is human-merged.

## Self-review

- **Spec coverage:** P3a-§3 → T3, T7; §4 → T4, T5, T6; §5.1–5.2 → T5, T6; §5.3 → T5 (pause, retry),
  T6 (order); §5.4 → T6 (AC-11); §6.1 → T2; §6.2 → T4; §7 → already in P1/P2 (rev 3); §9 → nothing
  built. AC-P3a-1…18 each named above (2, 3, 6–11, 14 in T6; 4, 5, 15–17 in T3/T7; 12, 13 in T5;
  18 in T4; 1 also T5).
- **D1 regression guard:** `rulings.rs:437` is not edited; AC-P3a-9 asserts the same count.
- **Type consistency:** `HumanNote`, `SendAnswer::Note`, `NoteLine`, `parse_note_line`,
  `unseen_notes`, `on_consented`, `REVIEW_HUMAN_NOTES_HEADER` spelled identically in every task.

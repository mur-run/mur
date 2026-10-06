# Agent review loop — Phase 3a design: human notes at the stdin send prompt

- **Status:** Draft rev 3. Decisions N1–N11 were taken in brainstorm (human, 2026-10-06).
  Rev 2 drops the reviewer "say why" instruction (§0), makes unknown slash commands ask again
  instead of stopping (N10), pins where `@<agent>` is resolved (N11), and names the
  partial-flush pause text (§5.3). Rev 3 applies §7 to the P1 and P2 specs in place. Rev 4
  (plan decision D1, 2026-10-06) keeps P2-§5.3's boundary `/rule` contract: the rebuilt
  message is printed and sent with no further prompt (§5.2, §5.3 retry, AC-P3a-9). Rev 5
  (2026-10-06) corrects AC-P3a-8 to P2-§5.3's held-ruling contract (§7). Two flagged
  assumptions remain (§0); no open questions.
- **Date:** 2026-10-06
- **Base:** Phase 1 spec `docs/superpowers/specs/2026-10-04-agent-review-loop-design.md` and
  Phase 2 spec `docs/superpowers/specs/2026-10-05-agent-review-loop-phase2-design.md`.
  `P1-§x` / `P2-§x` refer to them.
- **Owner (spec):** PM. **Build:** coding agent. **Verify:** QA. **Ship:** GitHub Manager.

---

## 0. Scope

Phase 2 left every P1-§6 human input except `/rule` unbuilt (P2-§0): `HumanNote` has a schema
variant but no writer, and it folds as a no-op (`ledger.rs:289`). Phase 3a gives the **stdin**
driver a way to send notes to the agents, broadcast or addressed, with the same
"what you see is what is sent" guarantee as P1's send prompt.

In scope:

- `/note <text>` (broadcast) and `@<agent> <text>` (addressed) at the stdin send prompt.
- The `@<unknown>` hint.
- Pending-note lifecycle: when a note reaches the channel, when it is discarded.
- Rebuilding the outgoing message from a generator, so the reprinted message is the one sent.
- The `unseen_notes` fold and the note block in the turn message.

Out of scope (later phases): MURMUR integration (plain-text notes, Esc bindings, footer hint),
Hub GUI, auto-mode `cost_usd`.

*Flagged assumptions:*

- The repeated-note `Make this a formal ruling with /rule?` prompt (P1-§6) is **not** built in
  3a. It needs to detect "the same finding raised twice in plain text", which has no defined
  matching rule yet.
- P1-§6 says a reviewer that does not adopt a note "must give a reason". 3a does **not** build
  this, and the reviewer message carries **no** instruction to give one. The verdict schema has
  nowhere to put a per-note reason (`prior[].reason` is keyed by finding id), so a model that
  obeyed such an instruction would invent a field, the `wire.rs` validator would reject the
  verdict as malformed, and the turn would retry and block. Building it needs note ids in the
  schema; that is a later phase.

## 1. Decisions

| # | Decision |
|---|---|
| N1 | stdin syntax differs from MURMUR on purpose. Text without a prefix still means **Stop** (unchanged; pinned by `session_tests.rs:391-392`). A broadcast note needs `/note <text>`. MURMUR (later) keeps P1-§6's plain-text-is-a-note behaviour. |
| N2 | `/note` and `@<agent>` never act as send consent. They record the note and ask again. Send consent is still only Enter, `y`, or `/rule` (P2-§5.3: `/rule` is the only input that replaces Enter). |
| N3 | `@<unknown>` prints `agent <name> not found; use /note <text> to send it to both sides` and asks again. It is never broadcast, not even with a hint. |
| N4 | `SendAnswer` gains `Note(HumanNote)`. The session parses the line and **returns** it. It does not ask again itself. `HumanNote.target` is kept as typed (`None` = broadcast, `Some(role)` = addressed); the session does not resolve a broadcast into one side. |
| N5 | The driver owns the pending-note queue (in memory) and rebuilds the message from `(ledger, pending)` before every prompt. The reprinted message and the sent message come from one function, so they are equal by construction. |
| N6 | The outgoing message is produced by a **generator**, not passed in as built `params`. This covers the first prompt, every malformed-reply resend prompt, and the `RuleFirst` rebuild. |
| N7 | Pending notes have three outcomes: `Go` → flush, `Stop` → discard, `RuleFirst` → keep (§5.2). |
| N8 | Flush is **at-least-once**: pending notes are appended to the channel after consent and **before** `transport.send`. Same delivery shape as rulings (`ledger.rs:419`). |
| N9 | Flush empties the queue. There is no "already appended" flag. |
| N10 | A line starting with `/` that is not a command of the send prompt prints `unknown command: <word>` and asks again. It never stops. Stopping stays `q`, plain text, or EOF. Today such a line stops, because `is_rule_command` is false and `is_send_answer` is false (`session.rs:351`). |
| N11 | `@<agent>` is resolved **in the session** (`TerminalGate`), not in the driver. The gate is given the two member names when it is built. `@主` / `@審查` are a fixed alias map to `Role::Main` / `Role::Reviewer` and never go through `canonicalize_agent_name`. |

## 2. Code facts this design relies on (`main` @ `731ce9fc`)

- `SendAnswer` has three variants, `driver.rs:72-77`: `Send`, `Stop`, `SendWithRuling(RulingInput)`.
- `run_turn` gates and sends in one function, `driver.rs:253-258`:
  `Gated::RuleFirst(r) => return Ok(TurnOutcome::RuleFirst(r)),` …
  `let reply = transport.send(member, params)?;`
  There is no hook between consent and send.
- A boundary `/rule` becomes `RuleFirst` and sends nothing, `driver.rs:233`:
  `SendAnswer::SendWithRuling(r) if g.boundary => Gated::RuleFirst(r),`
- The transport retry calls `run_turn` again with `pre_confirmed: true` (`driver.rs:320`), so it
  does not ask again.
- The malformed-reply resend computes its base text once, before the loop (`loop_driver.rs`):
  `let base = message_text(params).unwrap_or_default().to_string();` …
  `outgoing = text_message_params(&format!("{base}{hint}"));`
- The `RuleFirst` path rebuilds by hand with `main_turn_params(self.task, round, &ledger)`
  (`wire.rs:112`); the reviewer side uses `reviewer_turn_params` (`wire.rs:130`).
- `HumanNote { text, target: Option<Role> }` exists, `schema.rs:284-289`, and folds as a no-op,
  `ledger.rs:289`: `ReviewPayload::HumanNote { .. } => {}`
- `HumanNote` and `Ruling` carry no round (`payload_round` → `None`), so `fold_rounds` applies
  them to the sealed ledger and to any staged attempt; they survive the drop of an unsealed
  trailing round.
- `unseen_rulings[to]` is cleared on `TurnSent { to }`, `ledger.rs:191-192`.
- `canonicalize_agent_name` returns its input unchanged when nothing matches (P1-§6).

## 3. stdin syntax

At every send prompt (`Send to …? [Enter = send, q = stop]`), including resend prompts:

| Input | Session result |
|---|---|
| Enter, `y` | `SendAnswer::Send` |
| `q`, or any text not starting with `/` or `@` | `SendAnswer::Stop` |
| `/<word> …`, `<word>` not `rule` or `note` | hint `unknown command: /<word>`, ask again in the session (N10) |
| `/rule …` (valid) | `SendAnswer::SendWithRuling` (unchanged) |
| `/rule …` (invalid) | hint, ask again in the session (unchanged, `session.rs:285-288`) |
| `/note <text>` | `SendAnswer::Note(HumanNote { text, target: None })` |
| `/note` with empty text | hint `usage: /note <text>`, ask again in the session |
| `@<agent> <text>`, `<agent>` resolves to a session member | `SendAnswer::Note(HumanNote { text, target: Some(role) })` |
| `@主 <text>` / `@審查 <text>` | same, `Main` / `Reviewer` |
| `@<agent> <text>`, `<agent>` not a member of this session | N3 hint, ask again in the session |

Resolution of `@<agent>` (N11) happens in `TerminalGate::confirm_send`. `TerminalGate` gains the
two member names (`fleet.members[0]` = main, `[1]` = reviewer), passed where it is built
(`session.rs:422`, `session.rs:491`). Two separate paths, tested separately:

1. **Alias:** `@主` → `Main`, `@審查` → `Reviewer`. A fixed map, checked first; no name lookup.
2. **Name:** call `canonicalize_agent_name`, then the gate checks that the result equals one of
   the two member names. The function's return value alone is not proof of existence (it
   returns its input unchanged when nothing matches). An agent that exists on the machine but
   is not in this session is `<unknown>` here.

`@` with no name, or `@<agent>` with no text, prints `usage: @<agent> <text>` and asks again.

Unknown slash commands (N10) are checked after `/rule` and `/note`, so neither of those changes.
The existing pin `send_prompt_p1_answers_unchanged` (`session_tests.rs:391-392`) feeds `q`,
`nope` and EOF only, so it still holds.

The prompt help line gains `/note = note to both, @<agent> = note to one`.

## 4. Message generator (N5, N6)

`run_turn` (and the resend loop around it) stops taking built `&params`. It takes a generator:

```text
build: Fn(&Ledger, &[HumanNote]) -> serde_json::Value
```

- The driver calls `build(&ledger, &pending)` **before every prompt**: the first prompt, each
  malformed-reply resend prompt (base from the generator, then append `hint`), and for the
  `RuleFirst` rebuild (printed in full, then sent with no prompt; §5.2).
- Main uses `main_turn_params`, the reviewer uses `reviewer_turn_params`, both extended to take
  the pending slice. The hand-written rebuild in the `RuleFirst` path is replaced by the same
  generator; there is one way to build a turn message.
- What the generator renders for notes is §6.

## 5. Pending-note lifecycle

### 5.1 At the prompt

```text
loop {
    params = build(&ledger, &pending)
    match confirm_send(member, &params, open) {
        Note(n)          => { pending.push(n); continue }          // reprint, ask again
        Send             => Go
        SendWithRuling   => Go (non-boundary) | RuleFirst (boundary)
        Stop             => Stop
    }
}
```

Several `/note` lines in a row are each pushed, the message is rebuilt and reprinted after
each, and they are flushed in input order. The loop is bounded by the human: each `/note` is
one extra round trip; no retry cap is needed.

### 5.2 Outcomes

| Outcome | Trigger | Pending notes |
|---|---|---|
| `Go` | Enter, `y`, non-boundary `/rule` | flushed (§5.3), then sent with this message |
| `Stop` | `q`; text without a prefix; `.stopped`; stdin EOF | discarded; never reach the channel |
| `RuleFirst` | boundary `/rule` | **kept**. The driver writes the ruling, rebuilds the message from `(ledger, pending)`, prints it, and sends it. No further prompt (P2-§5.3: the `/rule` line was the send consent). Pending notes are flushed before that send. |

`RuleFirst` itself does **not** flush; the send that follows it does. The rebuilt message
carries the pending notes because the generator reads `pending`, not because they were
appended. Channel order is therefore `ruling`, then the notes, then `turn_sent`.

### 5.3 Flush (N8, N9)

`run_turn` gains a callback called once, after the gate returns `Go` and before
`transport.send`:

```text
on_consented: FnOnce(&mut Ledger) -> Result<()>
```

The driver's callback, for each pending note in queue order: `append_event(HumanNote)`, then
`ledger.apply`. When all are appended, it clears the queue. Only then does `run_turn` call
`transport.send`.

- **Order:** all notes are appended before the send; queue order is channel order.
- **Partial failure:** if appending note *k* fails, notes after *k* are not appended, nothing is
  sent, and the session pauses with `paused { kind: other }` (`PauseKind::Other`, wire value
  `"other"`, `schema.rs:212-218`; no new kind). The `reason` is a fixed constant, not the bare
  error, so the human can tell what is already on the channel:
  `note flush failed after <k-1> of <n> notes; the <k-1> already recorded will be sent on
  resume, the rest were not recorded: <append error>`. The same text is printed on pause. Without
  it, a human who sees the recorded notes again on resume would think they typed them twice.
  Notes already appended stay on the channel. No rollback, no deletion. This is the normal
  at-least-once state (§5.4).
- **Retry:** `on_consented` runs once per consented send, in `run_turn_with_retry` after the
  gate returns `Go`. The send after `RuleFirst` (`pre_confirmed: true`) is a consented send and
  does flush. The transport retry inside `run_turn_with_retry` does not call it again. Even if
  it did, the queue is already empty (N9).

### 5.4 Abort after consent

| Abort | Pending notes |
|---|---|
| before consent (`q`, plain text, `.stopped`, EOF) | discarded (§5.2) |
| after consent, before `TurnSent` (transport failure after retry, crash) | already on the channel. `unseen_notes[to]` is not cleared, so resume sends them again. |

Possible duplicate: a note was delivered but `TurnSent` was never written, so resume sends it a
second time. Accepted, same as rulings (§9).

## 6. Fold and message rendering

### 6.1 Fold

`Ledger` gains `unseen_notes: [Vec<HumanNote>; 2]`, mirroring `unseen_rulings`:

- `HumanNote { target: None }` → push to both slots.
- `HumanNote { target: Some(r) }` → push to `r`'s slot.
- `TurnSent { to }` → clear `unseen_notes[to]` (next to the `unseen_rulings` clear,
  `ledger.rs:191-192`). Notes for the other side stay.

`HumanNote` keeps carrying no round. It is never damage and never closes a finding.

### 6.2 Rendering

The generator renders, for the recipient `to`: `unseen_notes[to]` from the ledger, then the
pending notes whose target is `None` or `to`, in that order. Notes for the other side only are
not shown. If the list is empty there is no note block.

This order is the order **inside the message**. Channel order is always append order (queue
order, §5.3) and is never rearranged; the generator does not reorder or rewrite
`unseen_notes` to match the message.

The block outranks findings (P1-§6): it is placed before the findings section. Neither side's
block asks the agent to justify not adopting a note (§0).

On resume, `unseen_notes` is rebuilt by the fold from the channel, so the resumed prompt shows
notes that were appended but not yet delivered. The live and replayed ledgers must be equal.

## 7. Edits to earlier specs

*Applied in place in rev 3 (2026-10-06). Each earlier spec carries a Phase 3a status line
pointing back here; edited rows are marked *(Phase 3a)*.*

- **P1-§6:** mark `@<agent>` and `@<unknown>` as built in Phase 3a for stdin; change the
  `@<unknown>` hint text for stdin to N3's (no "sent as a general note"). Add a note that stdin
  uses `/note <text>` for plain-text notes (N1).
- **P2-§0:** the list of unbuilt inputs moves to "built in Phase 3a (stdin)", except the
  repeated-note `/rule` suggestion, the non-adoption reason, and MURMUR.
- **P2-§5.3 input table (lines 197-202):** add rows for `/note <text>`, `@<agent> <text>`, and
  unknown `/<word>` (ask again, N10), and narrow "anything else, or EOF" to "text not starting
  with `/` or `@`, or EOF". Otherwise P2 and P3a describe two different prompt grammars.
- **P1-§6 split by surface:** the plain-text and `@<unknown>` rows are kept for MURMUR (3b,
  original hint text unchanged) and get separate stdin rows (N1, N3). The `@<agent>` row notes
  that `@主` / `@審查` go through a fixed alias map, never `canonicalize_agent_name` (N11), and
  that only this session's two members match.
- **P2-§5.3** also lists the meaningful send-prompt prefixes explicitly (`/rule`, `/note`,
  `@<agent>`), so the grammar is not inferred from P3a.
- **AC-P3a-8 corrected (rev 5, found in plan Task 6):** rev 1–4 required the ruling from a
  non-boundary `/rule` to be on the channel before that `turn_sent`. That contradicts P2-§5.3
  (a `/rule` at the reviewer's send prompt is held and written after the round's seal) and
  AC-P2-18/19. P3a does not change ruling timing, for the same reason as D1: a ruling written
  before `turn_sent` would make the live message differ from the one replay rebuilds. P2 is
  unchanged; AC-P3a-8 now asserts the note half and the held ruling.

## 8. Acceptance criteria

- **AC-P3a-1:** `/note X` at main's prompt → message reprinted with X → Enter → channel order is
  `human_note(X, None)` then main `turn_sent`; the bytes sent equal the reprinted bytes.
- **AC-P3a-2:** after AC-P3a-1, the reviewer's next message contains X; main's message after
  that does not (cleared by main's `TurnSent`).
- **AC-P3a-3:** `@<reviewer-name> Y` at main's prompt → main's reprinted message does **not**
  contain Y; after Enter the channel has `human_note(Y, Reviewer)`; the reviewer's next message
  contains Y.
- **AC-P3a-4:** `@主 Z` and `@審查 Z` resolve to `Main` / `Reviewer` through the alias map;
  `@<NAME>` in a different case resolves through `canonicalize_agent_name`. Separate tests for
  the alias path and the name path.
- **AC-P3a-5:** `@nobody W` → hint `agent nobody not found; use /note <text> to send it to both
  sides`, prompt again, nothing on the channel. An agent that exists on the machine but is not
  in this session gives the same hint.
- **AC-P3a-6:** `/note A`, `/note B`, Enter → `human_note(A)` before `human_note(B)` before
  `turn_sent`; each reprint shows all notes so far.
- **AC-P3a-7 (Stop):** `/note A` then `q` → no `human_note` on the channel. Same for `/note A`
  then text without a prefix.
- **AC-P3a-8 (Go, non-boundary `/rule`) *(rev 5)*:** `/note A` then a valid `/rule` at a
  non-boundary prompt → `human_note(A)` is on the channel before that `turn_sent`, and the sent
  message carries A. The ruling is held per P2-§5.3 and written after the round's seal: it is
  not on the channel before that `turn_sent` and not in that message.
- **AC-P3a-9 (RuleFirst, boundary `/rule`):** `/note A` then `/rule` at a boundary prompt →
  `ruling` written, **no** `human_note` yet; the reprinted rebuilt message contains A and the
  ruling; no further prompt; channel order is `ruling`, `human_note(A)`, `turn_sent`; the sent
  bytes equal the reprinted bytes. `confirm:main` count is unchanged from P2
  (`mur-core/src/cmd/fleet/review/loop_driver_tests/rulings.rs` line 437).
- **AC-P3a-10 (resend):** malformed reply → resend prompt → `/note A` → reprint contains A and
  the format hint; Enter → the sent bytes contain A.
- **AC-P3a-11 (at-least-once):** `/note A`, Enter, transport fails twice → `human_note(A)` on
  the channel, no `turn_sent`, session paused; `review-resume` → the message to the same side
  contains A again.
- **AC-P3a-12 (retry, no double append):** `/note A`, Enter, first send fails, retry succeeds →
  exactly one `human_note(A)` on the channel.
- **AC-P3a-13 (partial flush):** two pending notes, the second append fails → first
  `human_note` on the channel, second not, no send, `paused { kind: other }` whose `reason`
  states 1 of 2 notes recorded (§5.3).
- **AC-P3a-14 (live == replay):** for AC-P3a-1, 3, 9, 10 and 11, and for a note flushed at the
  reviewer's prompt whose send then fails (the session stops mid-reviewer-turn), folding the
  channel from scratch gives the same `unseen_notes` as the live ledger.
- **AC-P3a-15:** `/note` with empty text → usage hint, prompt again, nothing returned to the
  driver.
- **AC-P3a-16 (unknown slash):** `/foo` and a typo `/riule drop F1 x` → `unknown command: …`,
  prompt again, not `Stop`; `send_prompt_p1_answers_unchanged` still passes unchanged.
- **AC-P3a-17 (non-member):** `@<agent>` where the agent exists on the machine but is not a
  member of this session → N3 hint, prompt again.
- **AC-P3a-18 (no reason instruction):** neither turn message contains an instruction to justify
  not adopting a note.

Test double: `RuleGate` (the scripted `confirm_send` used by the driver tests) must accept a
script containing `Note(..)` answers and must record the `params` it was shown at each prompt,
so AC-P3a-1, 9 and 10 can assert "reprinted == sent" against what the driver actually built.
A double that returns `Note` without being called again by the driver does not test N5.

## 9. Accepted limitations

These were decided, not deferred. Builders must not add handling for them; QA does not test them.

- **Duplicate delivery after a crash** between flush and `TurnSent` (§5.4). Same trade-off as
  rulings. A note is text, not a binding decision, so a duplicate has no state effect.
- **Reviewer non-adoption reason is not built** (§0). No instruction, no schema field.
- **No repeated-note `/rule` suggestion** (§0).
- **No MURMUR surface.** 3a is stdin only.

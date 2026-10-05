# Agent review loop — Phase 2 design: escalation → ruling

- **Status:** Draft rev 4. Decisions R1–R9 were taken in brainstorm (human, 2026-10-05). R7
  (proactive `/rule`) was recommended and taken as a flagged assumption under autonomous
  continuation. Rev 4 applies the plan review rulings D1–D3 (human, 2026-10-05): `/rule`
  timing at the send prompts (§5.3), one resume column for paused and crashed sessions (§6),
  and no plain-text notes in Phase 2 (§0, R7, §9). No open questions remain; accepted
  limitations are in §9.
- **Date:** 2026-10-05
- **Base:** Phase 1 spec `docs/superpowers/specs/2026-10-04-agent-review-loop-design.md` (Approved
  rev 3), implemented on `main` by #1722 (`400b1354`). Section numbers `P1-§x` refer to it.
- **Owner (spec):** PM. **Build:** coding agent. **Verify:** QA. **Ship:** GitHub Manager.

---

## 0. Scope

Phase 1 treats escalation as a **Stop**: the loop ends, `session_stopped` is written, the fleet
definition is removed, and the session cannot be resumed. `/rule` is specified (P1-§6) but never
written by any code path. Phase 2 makes escalation a **wait for a human ruling** and gives `/rule`
real semantics.

Out of scope: `cost_usd` work for auto mode (Phase 3 blocker), reviewer appeals, multi-finding
rulings, Hub GUI, **MURMUR integration**, and the other P1-§6 human inputs: plain-text
`human_note`, `@<agent>` / `@主` / `@審查` addressed notes, the `@<unknown>` hint, and the
repeated-note `/rule` suggestion. None of these has an input path or a writer on `main`
(`HumanNote` folds as a no-op, `ledger.rs:198`); they move to Phase 3 together with MURMUR.
`/rule` is Phase 2's only way for a human to intervene. Phase 2's human surface is the terminal (stdin)
driver that Phase 1 already ships (`session.rs`: `Send to …? [Enter = send, q = stop]`,
`Paused — continue? [Enter = continue, q = leave paused]`). Wiring `/rule` into MURMUR's slash
table comes later and must reuse the event and fold rules here unchanged.

## 1. Decisions

| # | Decision |
|---|---|
| R1 | No new top-level session state. Waiting for a ruling while detached is `paused` with `kind = escalation`. The P1-§7.0 state table is unchanged. |
| R2 | Whether a ruling is owed is **derived from the ledger**, never from the `paused` event: a session owes a ruling iff it has an escalation with `handled == false`. This holds for paused **and** crashed sessions. |
| R3 | Escalation is always **derived by the fold** from `rebuttal` events. The driver never writes an `escalation` event. |
| R4 | One ruling addresses exactly one finding: `Ruling { finding, decision: drop \| fix, text }`. |
| R5 | `drop` = the main agent is right; the finding closes. `fix` = the reviewer is right; the finding stays open and the main agent may no longer reject it. |
| R6 | An escalation is handled iff a ruling **names its finding** and appears after it in channel order. It does not depend on whether the ruling closes the finding. |
| R7 | *(assumption, flagged)* `/rule` is accepted for **any open finding**, not only escalated ones. A proactive ruling folds identically; there is just no escalation to mark handled. Reason: Phase 2 has no plain-text note (§0), so `/rule` is the only way a human can intervene before an issue reaches escalation. |
| R8 | At the ruling prompt — the single prompt shown both live (§5.1) and by `review-resume` (§6); there is no second variant — `q` means **leave paused**, as at the P1-§7.0 pause prompt. Abandon is the explicit `/abandon`, never a single key, because it is destructive (`session_stopped`, fleet definition removed, no resume). |
| R9 | Re-raising a dropped issue under a **new** finding ID is not detected in Phase 2 (accepted limitation, §9). |

## 2. Code facts this design relies on (`main` @ `400b1354`)

- Escalation is computed inside the `rebuttal` fold, `mur-core/src/cmd/fleet/review/ledger.rs:189`:
  `if f.reject_count == REJECT_ESCALATION_THRESHOLD {`
- The driver checks escalation only after the round's `verdict` seal,
  `mur-core/src/cmd/fleet/review/loop_driver.rs:318-322`:
  `ledger.note_round_complete();` … `VerdictKind::Revise if !ledger.escalations.is_empty() => {`
  Therefore every escalation belongs to a sealed round, and P1-§3.3.1 sealing needs no change.
- `ReviewPayload::Escalation` is read by the fold (`ledger.rs:206`) and written nowhere.
- `ReviewPayload::Ruling { text, closes }` (`schema.rs:268`) is read by the fold (`ledger.rs:199`)
  and written nowhere, so no retained channel contains a `Ruling` and its shape can change freely.
- `escalations` only grows; `Ruling` never touches it. **Bug:** after any ruling the next `revise`
  still sees a non-empty list and stops again.
- `Ruling` and `HumanNote` carry no round (`schema.rs:326` `payload_round` → `None`), so the fold
  applies them to both the sealed ledger and any staged attempt (`ledger.rs` `fold_rounds`). A
  ruling therefore survives the drop of an unsealed trailing round.
- `Paused { reason: String }` is free text today (e.g. `transport failure after one retry: …`,
  `driver.rs:240`).
- Duration-stuck is measured from `last_activity`, set only after an agent turn returns
  (`loop_driver.rs:395`) and checked at the top of each round (`loop_driver.rs:244`).

## 3. Schema changes

### 3.1 `Ruling`

```rust
Ruling {
    /// The single finding this ruling addresses (`F<n>`).
    finding: String,
    decision: RulingDecision, // Drop | Fix
    text: String,
}
```

`closes: Vec<String>` is removed. Safe because no `Ruling` has ever been written (§2).
`REVIEW_SCHEMA_VERSION` stays 1 for the same reason.

### 3.2 `Escalation`

`ReviewPayload::Escalation` is **removed** from the enum (R3). A review note that still carries it
fails to parse and is damage per P1-§8.2. None exists, because nothing ever wrote one.

### 3.3 `Paused`

Add a machine-readable kind, keep the free-text reason for display:

```rust
Paused {
    #[serde(default)]          // Phase 1 events parse as `Other`
    kind: PauseKind,           // Escalation | Transport | Detached | User | Other
    reason: String,
    #[serde(flatten)] cumulative: Cumulative,
}
```

`kind` is **display only** (resume prompt wording). Resume correctness never reads it (R2).

### 3.4 Derived ledger fields (not events)

- `EscalationRecord.handled: bool`
- `Finding.ruled: Option<RulingDecision>` — the decision of the **last** ruling naming it.
- `Finding.reject_count` — counts only rejects **after** the last ruling naming the finding.

## 4. Fold rules

Applied in channel order. Round sealing (P1-§3.3.1) is unchanged.

1. **Rebuttal `reject` on finding F:**
   - if `F.ruled == Some(Fix)`: the fold does nothing. (The driver rejects such a reply as
     malformed before it is written — §5.3 — so this is defence in depth, not a path.)
   - else `F.reject_count += 1`; when it reaches `REJECT_ESCALATION_THRESHOLD` exactly, push
     `EscalationRecord { finding_id: F, handled: false, .. }`. Unchanged from Phase 1 except for
     the reset below.
2. **Ruling on F:**
   - mark every `EscalationRecord` for F with `handled == false` as handled;
   - `F.reject_count = 0`; `F.ruled = Some(decision)`;
   - `drop` → `F.status = Resolved`; `fix` → `F.status = Open` (a `disputed` finding returns to
     `open`).
   - A ruling naming a finding that was never issued is damage (P1-§8.2), like
     `StatusForUnissuedFinding`.
3. **Reviewer `finding_status` on a finding with `ruled == Some(Drop)`** to anything other than
   `resolved` is an illegal transition (damage). The reviewer may not reopen a dropped finding.
4. **Reviewer `finding_status` on a finding with `ruled == Some(Fix)`**: only `open` or `resolved`
   is legal; `disputed` is illegal.

Consequences:

- The same finding cannot escalate twice: `drop` closes it, and after `fix` its rejects are
  malformed. Only a **new** finding can escalate again. After `fix` the disagreement can only end
  in `accept`, `partial`, or round-stuck (P1-§3.3).
- `reset` is not an event. Adding one later would create two sources of truth for `reject_count`.

**Single definition.** `ledger.pending_ruling()` = the escalations with `handled == false`. The
driver's stop check, the resume decision, and the stop/resume screens all call this one function.

## 5. Driver behaviour

### 5.1 On escalation (human attached)

After the sealing `verdict` of a `revise` round, if `pending_ruling()` is non-empty:

1. Force semi-auto. Phase 2 has no auto mode (Phase 3), so this is a no-op that Phase 2 never
   observes; it is kept so Phase 3 writes `mode_changed` here rather than inventing a new hook.
2. Write nothing else. The session stays `running`; the driver keeps the run lock.
3. The terminal prints both sides' last positions on F3, then the prompt
   `Awaiting ruling on F3 — /rule drop|fix F3 <text>, /abandon, q = leave paused`.
4. When the prompt returns (a line was read, or EOF), check `mur fleet stop` (the `.stopped`
   kill-switch) **first**, before parsing the input or writing anything, with the same
   cooperative semantics as P1-A4: the blocking read is not interrupted. If set, the input is
   discarded and the session ends exactly as `/abandon`, except the reason is the existing
   kill-switch stop (P1 behaviour), not `escalation`.
5. Only if the kill-switch is not set, parse the input. Exactly one branch applies:
   - `/rule …` (valid): append `ruling`, re-fold; if `pending_ruling()` is empty continue with
     the next round, otherwise prompt for the next pending escalation.
   - `/abandon`: append `session_stopped { reason: escalation }`, remove the fleet definition,
     show the P1-§8.3 stop screen. Not resumable.
   - `q` or EOF: §5.2.
   - anything else (including an invalid `/rule`): re-prompt with an inline hint; nothing is
     written.

Time spent at this prompt is **human-input wait** (P1-§3.5): excluded from `deadline`, recorded in
the next `turn_sent.human_wait_ms`. Writing a `ruling` **resets the duration-stuck activity clock**
(`last_activity`); otherwise a ruling that takes longer than `limits.stuck` trips `stuck: no
activity` the moment the loop continues.

### 5.2 On detach while awaiting a ruling

`q` at the ruling prompt, or stdin EOF (the terminal closed; `is_send_answer` already treats an
empty read as EOF) → append `paused { kind: escalation }`, release the lock, keep the fleet
definition. Standard P1-§7.0 pause. Exception: at the `review-resume` prompt nothing is written,
because the session is already paused there — by its own `paused` event, or by the
`paused { kind: other, reason: "crashed" }` that resume writes before the prompt (§6,
AC-P2-17).

A process killed without reaching EOF (SIGKILL, SIGHUP without a clean read) writes nothing and is
classified `crashed`; §6 still finds the pending ruling (R2).

### 5.3 Turns after a ruling

- The ruling text is injected as a **binding note** into the next turn of **both** sides, ranked
  above findings.
- `fix` on F: in its rebuttal the main agent may answer F only with `accept` or `partial` (reason
  required). `reject` → malformed → retry once → `blocked` (P1-§3.4 rule).
- **`/rule` outside the ruling prompt is read only at a send prompt** (`Send to …?`). The
  stdin driver does not read input while a turn is in flight; a line typed then is read by the
  next prompt like any other input. Input at a send prompt:

  | Input | Effect |
  |---|---|
  | Enter (`y`, `yes`) | send (P1) |
  | valid `/rule …` | **send and record a ruling**; this turn is still sent |
  | invalid `/rule …` (§5.4) | inline hint, re-prompt; nothing recorded or sent |
  | anything else, or EOF | unchanged from P1 (stop) |

- **A ruling is applied only at a round boundary**: before main's `turn_sent`, so on the channel
  every `ruling` precedes the main `turn_sent` of the round it governs. Reason: the reviewer's
  turn is mid-round — main's rebuttal is folded in memory but written only at the seal — so a
  ruling written then would replay before the rebuttal while the live ledger applied it after
  (AC-P2-10).
  - At **main's** send prompt the prompt *is* the round boundary. The kill-switch is checked
    first (§5.1 step 4); then the `ruling` is written and folded at once, main's message is
    **rebuilt from the new ledger** (binding note, changed open set, post-`fix` restriction) and
    printed, and it is sent without asking again — the `/rule` line was the send consent.
    Sending the message built before the ruling would make main answer under the old rules.
  - At the **reviewer's** send prompt the reviewer turn is sent; the ruling is held and written
    after this round's seal, before the next round's main `turn_sent`.
- **A held ruling is re-validated when applied**: if its finding is still in the open set
  (`open` ∪ `disputed`, P1-§3.3) it is written and folded — a finding the reviewer moved to
  `disputed` meanwhile is still ruled, which is what the ruling is for. If the finding left the
  open set (`resolved` or `withdrawn`) the ruling is discarded, one line is printed
  (`Ruling on F3 not recorded: F3 is now withdrawn.`), and nothing is written. A reviewer
  closing a finding first is a normal race, not a failure; the loop continues.

### 5.4 Proactive `/rule` (R7)

Same parsing and fold as §4. Valid for any finding in the open set; a ruling on a closed (`resolved`,
`withdrawn`) or unknown finding is refused at input with an inline hint and never written.

## 6. Resume

`mur fleet review-resume` runs the fold, then branch on the **ledger**, not on the
last event kind:

| Ledger after fold | Prompt |
|---|---|
| `pending_ruling()` non-empty (paused **or** crashed) | the §5.1 ruling prompt — kill-switch check and inputs below |
| no pending ruling, last event `ruling` with no later `resumed` | `Ruling recorded — continue?` |
| otherwise paused | `Paused — continue?` (unchanged) |
| otherwise crashed | P1-§7.0 *Crashed* path (unchanged) |

A crash while awaiting a ruling leaves no `paused` event and is classified `crashed` by P1-§7.0.
Because escalation is re-derived from sealed `rebuttal` events (R3), the crashed path still finds
the pending ruling and **cannot skip it**.

For a crashed session, resume writes `paused { kind: other, reason: "crashed" }` **after taking
the run lock and before showing the prompt** — the same event the P1 crashed path writes
(`resume.rs:231`), only earlier. Writing it after the prompt would close the execution-time
segment at the end of the human's wait, so `active_time` would count that wait. Once written,
a crashed session is an already-paused session and one table covers both.

At the ruling prompt reached through `review-resume`, §5.1 steps 4–5 apply unchanged, with these
resume-specific effects (the run lock is taken before the prompt is shown):

| Input | Effect (paused, or crashed after the pre-prompt `paused`) |
|---|---|
| kill-switch set | P1 kill-switch stop |
| valid `/rule` | append `ruling`; when none is pending append `resumed` and continue |
| `/abandon` | `session_stopped { reason: escalation }`, fleet definition removed |
| `q` or EOF | **write nothing**, release the lock; a second `paused` would be a duplicate |
| anything else | re-prompt, nothing written |

`/abandon` is legal on both the live and the resume path; a builder must not wire it to the live
driver only.

## 7. Edits to the Phase 1 spec

| P1 section | Change |
|---|---|
| §3.4 | "the system escalates to the human automatically" → "…escalates; the loop waits for a ruling (Phase 2 §5)". Add the post-`fix` answer restriction. |
| §3.5 | Remove `escalation` from "The loop stops on". |
| §4 | Remove `escalation` from the event list; add the note "escalation is derived by the fold, never written". Change `ruling` fields. Add `kind` to `paused`. |
| §5 | "Forced back to semi-auto **and stop** on: `blocked`, escalation, …" → "Forced back to semi-auto on escalation **and await a ruling**; forced back and stop on `blocked`, a limit trip, transport failure." |
| §6 | `/rule` row → `/rule drop\|fix F<n> <text>`, one finding per ruling, any open finding; outside the ruling prompt, read at the send prompts (P2-§5.3). Plain-text, `@<agent>`, `@<unknown>` and repeated-note rows: mark *not built; Phase 3* (P2-§0). |
| §7 / §7.0 | *Pause is not stop* table: move escalation out of **Stop**. Escalation now ends in Pause (`q` or EOF, §5.2) or in Stop only by explicit `/abandon` or the kill-switch (§5.1). State table unchanged. Resume branches per §6 above. |
| §8.3 | Stop screen no longer lists `escalation` as a stop reason (only `/abandon` at the ruling prompt, §5.1). |
| AC8 | "produces an `escalation` event" → "produces a pending escalation in the folded ledger; no `escalation` event is written". |
| AC18 | Replaced by AC-P2-3/4. |

## 8. Acceptance criteria

- **AC-P2-1:** F rejected twice → loop waits at the ruling prompt; no `session_stopped`, no
  `escalation` event on the channel, fleet definition present, lock held.
- **AC-P2-2:** after `/rule drop F` the next `revise` does **not** stop on escalation (regression
  for the `escalations` accumulation bug).
- **AC-P2-3:** `/rule drop F` → F `resolved`; a later reviewer `finding_status` reopening F is
  damage.
- **AC-P2-4:** `/rule fix F` → F `open`, `reject_count == 0`, escalation handled; a main `reject`
  of F is malformed → retry → `blocked`; `partial` with reason is accepted and does not count.
- **AC-P2-5:** SIGKILL while awaiting a ruling → `review-resume` writes `paused { kind: other,
  reason: "crashed" }`, then shows the ruling prompt, not the crashed continue path; time at
  that prompt is not counted in `active_time`.
- **AC-P2-6:** detach while awaiting → `paused { kind: escalation }` → `mur fleet review-resume` shows the ruling
  prompt → `/rule` → `resumed` → next round.
- **AC-P2-7:** 15 min at the ruling prompt with `stuck = 10m`, `deadline = 5m` → neither trips.
- **AC-P2-8:** `/rule` at a send prompt. (a) At main's prompt: `ruling` is on the channel before
  that round's main `turn_sent`, and the message sent to main is the one rebuilt after the
  ruling (it contains the binding note). (b) At the reviewer's prompt: the reviewer turn is
  sent; `ruling` lands after that round's `verdict` and before the next round's main
  `turn_sent`. (c) In both, the `/rule` line also counts as send consent.
- **AC-P2-9:** proactive `/rule fix F` with no escalation folds identically to AC-P2-4.
- **AC-P2-10:** replay equals live ledger byte-for-byte across rulings (extends P1 AC11 property
  test with `Ruling` events and the new derived fields).
- **AC-P2-11:** a Phase 1 `paused` event without `kind` parses as `Other`.
- **AC-P2-12:** `q` at the ruling prompt → `paused { kind: escalation }`, fleet definition kept;
  no `session_stopped`.
- **AC-P2-13:** `/abandon` at the ruling prompt → `session_stopped { reason: escalation }`, fleet
  definition removed, `review-resume` refuses.
- **AC-P2-14:** `mur fleet stop` while at the ruling prompt → after the next input the session
  stops as in P1 (kill-switch), and no `ruling` from that input is written.
- **AC-P2-15:** stdin EOF at the ruling prompt behaves as `q` (AC-P2-12).
- **AC-P2-16:** `/abandon` at the ruling prompt reached through `review-resume` (from both a
  paused and a crashed session) → `session_stopped { reason: escalation }`, fleet definition
  removed.
- **AC-P2-17:** `q` at the `review-resume` ruling prompt writes no event after the prompt: for a
  paused session the channel tail is unchanged; for a crashed one the only new event is the
  pre-prompt `paused`.
- **AC-P2-18:** a ruling held from the reviewer's send prompt is re-validated when applied. The
  reviewer's turn in that round moves its finding to (a) `resolved` → ruling discarded, notice
  printed, no `ruling` on the channel, loop continues; (b) `withdrawn` → same as (a);
  (c) `disputed` → `ruling` written before the next main `turn_sent` and folded (`fix` →
  `open`, `drop` → `resolved`).

## 9. Accepted limitations

These were decided, not deferred. Builders must not add handling for them; QA does not test them.

- **Re-raise after drop (R9).** Fold rule 3 stops the reviewer reopening a dropped **finding ID**,
  but cannot stop it issuing a **new** finding that restates the same issue. Detecting that needs
  semantic comparison. Phase 2 relies on the binding note and the human's ability to
  `/rule drop` the new ID (proactive `/rule`, R7). There is no repeated-note prompt in Phase 2
  (§0). Revisit in Phase 3 only if real use
  shows it is a problem.
- **No MURMUR surface.** Phase 2 is stdin only (§0). A detach in Phase 2 means stdin EOF.

# murmur proposal chip — agent-proposed commands and actions (design)

**Date:** 2026-09-28
**Status:** Design approved (Q1–Q7 settled); tool contract and vetting location settled in §1; PR 2 built
**Scope:** `mur agent cli` (murmur) TUI. Lets an agent *propose* a command or a
native action; the user decides with one key. First native action: `/restart`.
Builds on the `suggest_replies` tool
(`2026-06-28-mur-agent-cli-suggested-replies-design.md`).

## Goal

Today an agent that needs the user to run something prints it as prose
("restart the agent to apply (mur agent stop <name>, then start it again)").
The user retypes it. This design turns that into a structured proposal that
murmur renders as a chip under the composer:

- **insert-only** proposals (`shell`, `slash`) — `Tab` puts the text in the
  empty composer; the user reviews and sends it themselves.
- **executable** proposals (native actions, v1 = `restart`) — `Enter` on an
  empty composer runs it in-process.

**Principle (C):** a proposal never runs anything the user has not seen as the
thing being run. Insert-only proposals can *never* be sent by a single key.

## Settled decisions

| # | Question | Decision | Why |
|---|---|---|---|
| Q1 | Shape of a proposal | Structured `Proposal { label, kind }`; `kind` is `Insert(Shell\|Slash)` (insert-only) or `Action(Restart)` (executable) | Today's hints are free text — `RESTART_HINT` even ships a literal `<name>`. With a type, "can this run?" is decided by the variant, not by parsing a string |
| Q3 | Where proposals are vetted, what is rejected | A pure function at tool-call ingress. Rejected proposals return a tool error to the agent and never reach the UI. Rejects: newlines / control chars, unresolved placeholders (`<name>`, `{agent}`), secret-shaped strings, actions outside the allowlist, actions targeting another agent | Stop bad proposals at the earliest point so they are never rendered; a pure function is trivially unit-testable |
| Q4 | Native action allowlist | v1: `restart`, current agent only | One-key execution starts from the smallest set; each new action is an explicit allowlist extension |
| Q5 | Can a shell proposal run on Enter? | **No.** Shell behaves like slash: Tab-insert into an empty composer, the user reviews and sends | Principle (C): nothing the user has not seen as the thing being run executes on a single key |
| Q2 | What happens to an existing draft? | Never moved, hidden, or overwritten | Moving text out of sight is surprise |
| Q6 | Tab with a draft in the composer | **A:** Tab inserts only when the composer is empty; with a draft, Tab keeps opening slash completion and the chip hint reads `(清空後 Tab 插入)` / "(clear to Tab-insert)" | Identical to the existing ghost rule; cursor insertion (B) breaks `!` shell mode and steals slash-completion Tab; stash-the-draft (C) is what Q2 rejected |
| Q6b | Merge ghost into chip now? | **No — separate PR 3** | Keeps PR 2 reviewable and revertible |
| Q7 | `/restart` while a turn is streaming | **A:** not accepted. Hint reads `(回合結束後 Enter 執行)`; Enter is inert until the turn ends | Restart drains in-flight turns — the in-flight turn is the one on screen, so allowing it invites a self-deadlock. Queueing (B) is a hidden delayed action; aborting (C) drops output |

## §1 Proposal model and vetting

```text
Proposal { label, kind }
kind = Insert(Shell(cmd) | Slash(cmd))        // insert_only
     | Action(Restart)                        // executable, always the proposing agent
```

**Tool contract (settled).** The agent proposes through a dedicated tool,
`propose` (`mur_common::proposal::PROPOSE_TOOL`), separate from the no-op
`suggest_replies`. Input schema, `additionalProperties: false`:

| Field | Type | Rule |
|---|---|---|
| `label` | string | required; ≤ `LABEL_MAX_CHARS` (80) |
| `kind` | `"shell"` \| `"slash"` \| `"restart"` | required |
| `command` | string | ≤ `COMMAND_MAX_CHARS` (500); required for `shell`/`slash` (a leading `!`/`/` is accepted and stripped); **forbidden** for `restart` |

**Where vetting lives (settled).** `Proposal` and
`vet(&serde_json::Value) -> Result<Proposal, VetError>` live in
`mur-common` (`mur_common::proposal`). Both sides call the same pure function:
the runtime's tool executor (rejection → `ToolError::InvalidInput`, so the
agent sees why) and murmur (renders only what passes). It cannot live in
`mur-core`: `mur-agent-runtime` must not depend on it.

Vetting is a pure function run when the tool call arrives; a rejected proposal
returns a tool error to the agent and never reaches the UI. Reject:

- newlines or control characters
- unresolved placeholders (`<name>`, `{agent}`, …)
- secret-shaped strings
- a `kind` outside the allowlist (`shell`, `slash`, `restart`)
- a `command` on `restart` — `restart` carries no target, so a proposal aimed
  at another agent cannot be expressed (stricter than rejecting one; replaces
  the earlier "reject a foreign target" rule)

**As built (PR 2):** the agent proposes via a dedicated `propose` tool
(`{label, kind: shell|slash|restart, command?}`), separate from the no-op
`suggest_replies`. `Proposal` and `vet()` live in `mur_common::proposal` so the
runtime (tool executor → `ToolError::InvalidInput` on rejection) and murmur
(renders only what passes) run the same pure function. `restart` takes no
target at all — a proposal aimed at another agent cannot be expressed, which is
stricter than rejecting one.

## §2 Key dispatch

Chip state is **its own field**, not part of `completion`. Reason: Enter in the
completion overlay accepts *and sends* a leaf candidate
(`cli/events.rs:502-504`: "Enter accepts the candidate; if it is a leaf (no
submenu) we also send right away instead of forcing a second Enter"). Riding
on that state would let one Enter send an insert-only proposal — breaking C.

Priority, highest first: **HITL > completion overlay > chip > ghost > default.**
HITL handles Esc and returns first (`cli/events.rs:404`), so the chip can never
swallow an approval denial.

| Key | Condition | Behavior |
|---|---|---|
| Tab | empty composer, chip present | insert proposal text (`set_input` — safe only because the composer is empty) |
| Tab | draft present | unchanged: opens slash completion; chip untouched |
| Enter | empty composer, `executable`, idle | run the action |
| Enter | empty composer, `insert_only` | **nothing** — not sent, not inserted |
| Enter | draft present | sends the draft; chip stays |
| Enter | streaming, `executable` | inert (Q7) |
| Esc | idle + empty composer, chip present | dismiss chip (this slot is `EscAction::Nothing` today, `cli/app/keys.rs:30/37`) |
| Esc | streaming or draft | unchanged double-Esc state machine (`esc_action`, `cli/app/keys.rs:17`) |

The chip is also cleared when a newer proposal replaces it or a new turn
starts (same place the ghost is cleared today, `cli/turn.rs:7`).

`cli/events.rs` is 685 lines; key handling lives in a new `cli/proposal.rs`,
with `events.rs` gaining only a dispatch hook (Rule 5, ≤ 800 lines).

## §3 `/restart` wrapper

**No reconnect step is needed.** murmur dials fresh every turn
(`dial_message_streaming` spawned per turn, `cli/stream.rs:228-230`), so the
next turn reaches the new process on its own.

**Conversation survives.** murmur threads the prior turn id as
`context.task_id` (`cli/stream.rs:177-178`: "threading the previous turn's task
id as `context.task_id` so the agent keeps conversation history"). The runtime
keys `ConversationStore` by it, and the store is disk-backed — see the
`ConversationStore` doc comment in `mur-agent-runtime/src/task_runner.rs`
("Backed by disk when `dir` is set (issue #1199): a restart used to drop every
conversation…"), with production passing
`Some(agent_home.join("conversations"))` (`supervisor_runner.rs:650`). Only
in-flight tasks are lost (`cli/turn.rs:261`: "tasks live in memory only") —
which Q7 already rules out.

Two hazards in `restart_one` (`cmd/agent/restart.rs`, private, 624 lines):

1. **It prints.** 5 `println!`/`eprintln!` inside `restart_one`, e.g.
   `restart.rs:394`: `println!("agent '{name}' has no service installed; respawning runtime directly");`.
   Writing stdout under the TUI corrupts the screen. Fix: a pure core that
   returns `RestartReport` plus a message list; the CLI prints them, murmur
   renders them as `Msg`s.
2. **It blocks** (`thread::sleep` polling up to `stop_timeout` + respawn).
   murmur runs it on a `std::thread` and receives the result over a channel,
   same shape as `stream.rs`.

Flow: chip `⤷ /restart mur` → Enter → worker thread runs the pure
`restart_one` → footer shows `restarting mur…` and **turn submission is locked**
→ `✓ mur restarted (pid A → B)` or `✗ <detail>` → unlock.

## §4 Rewriting restart hints

Five sites in the TUI path. CLI-only outputs (`doctor.rs`, `perm.rs`,
`a2a_dial.rs`, …) never reach murmur and are out of scope.

| Site | Today | After |
|---|---|---|
| `cli/manage.rs:132/138/155/160` (`RESTART_HINT`) | hint appended to the returned string | functions return `(String, Option<Proposal>)`; string drops the hint; caller hands the proposal to the chip |
| `cli/app/transcript.rs:100` | "restart it (mur agent restart {agent}) for the step view" | keep the explanation, attach the same `Restart` proposal |

**Latent bug:** `cli/manage.rs:16-17`

```rust
pub const RESTART_HINT: &str =
    "profile updated — restart the agent to apply (mur agent stop <name>, then start it again)";
```

carries a literal `<name>` and teaches the old two-step stop/start. §1 vetting
would reject it as an unresolved placeholder — evidence it belongs as a
structured proposal, not a string. Fixed as part of PR 2.

## §5 Tests

**Vetting (pure, unit):** newline / control char / `<name>` / `{agent}` /
secret-shaped → rejected with a tool error; unknown kind, or `restart` with a
`command` → rejected; `shell`/`slash` → insert_only; `restart` → executable.

**Keys (state machine, no TTY):**
- empty + executable + idle + Enter → restart fires
- empty + insert_only + Enter → nothing sent, nothing inserted (**core C
  regression test**)
- draft + Enter → draft sent, chip kept
- draft + Tab → slash completion opens, draft unchanged
- streaming + Enter/Esc → chip untouched, existing semantics hold
- completion overlay open → overlay wins
- HITL open + Esc → approval denied, chip unaffected

**Restart:** pure `restart_one` writes nothing to stdout (capture and assert
empty); submission locked during restart and unlocked after;
`context_task_id` unchanged across restart.

## PR split

1. **Pure move.** Extract a non-printing `restart_one` returning
   `RestartReport` + messages; make it `pub(crate)`. CLI output byte-identical.
   No behavior change.
2. **Feature.** Proposal model, vetting, `cli/proposal.rs`, `/restart` wrapper,
   §4 rewrites (incl. the `RESTART_HINT` bug), §5 tests.
3. **Refactor.** Fold ghost into chip (a ghost is an insert-only chip).
   Acceptance: Tab inserts only into an empty composer and never touches a
   draft; existing ghost tests pass **unmodified**.

## Out of scope

- Native actions beyond `restart`.
- CLI (non-TUI) restart hints.
- Hub GUI rendering of proposals.

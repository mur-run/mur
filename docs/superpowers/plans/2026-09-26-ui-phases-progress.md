# UI phases 5/7/8/9 — working notes

Scope handed to dev_ui: `/memories` two-section UI (§11), blocking UX for
`RequiredBudgetExceeded` (§9), migration notice (§10), delete/demote
confirmation (§7). **injector.rs step 4 is explicitly out of scope.**

## Build reality (measured, not assumed)

- `protoc` cannot execute → any crate pulling `lance-encoding` (i.e.
  `mur-core`) cannot be compiled or tested in this sandbox.
- `mur-compress` DOES build and test, but only with the Command Line Tools
  toolchain. The `export` line in `memory_budget.rs`'s header comment is
  subtly wrong — it expands `$DEVELOPER_DIR` before setting it. Correct form:

  ```sh
  export DEVELOPER_DIR=/Library/Developer/CommandLineTools
  export CC="$DEVELOPER_DIR/usr/bin/clang"
  export CXX="$DEVELOPER_DIR/usr/bin/clang++"
  export SDKROOT="$DEVELOPER_DIR/SDKs/MacOSX.sdk"
  ```

  With that, `cargo test -p mur-compress` = 124 passed, 0 failed.

## State found on arrival

Already implemented by earlier phases:
- `memory_budget.rs` — budget, projection, §7 decision table, canonical counter.
- `memory_ux.rs` — `usage_summary` meter.
- `memory_cmds.rs` — two-section `memories()`, `instruct`, `pin`, `unpin`,
  `forget`, `MemoryOutcome::Confirm`, `apply_pending`.

So §11 and the §7/§9 *decision logic* are done. What is missing is the
**UI wiring**: none of `/instruct`, `/pin`, `/unpin`, `/instruct-edit` exist as
slash commands, so all of that logic is unreachable from the TUI.

## Gaps this pass addresses

| # | Gap | Crate | Verifiable here |
|---|---|---|---|
| 1 | `forget`/`unpin` return `MemoryOutcome`; callers still treat as `String` → **build break** | mur-core | no |
| 2 | No `/instruct`, `/pin`, `/unpin`, `/instruct-edit` in `SlashCmd` | mur-core | no |
| 3 | No confirmation UI for Delete/Demote (logic exists, unreachable) | mur-core | no |
| 4 | No pre-send budget gate → overflow cannot block a turn | mur-core | no |
| 5 | Blocking-overlay copy + migration notice text | mur-compress | **yes** |
| 6 | `instruct_edit` (§7's "Save anyway") not implemented | mur-core | no |
| 7 | help text + completion entries for the new commands | mur-core | no |

Gap 5 is the only one with a compilable test, so it is written test-first.
Everything else is written carefully and listed under "needs verifying".

## What was done

Verified by `cargo test` (135 passed, 0 failed in `mur-compress`; 115 config
tests in `mur-common`):
- `mur-compress/src/memory_block.rs` — NEW. `blocked_send_overlay`,
  `blocked_send_overlay_if_blocked`, `migration_notice`.
- `mur-compress/tests/memory_block_ux.rs` — NEW, 11 tests. Written red-first
  (confirmed failing on an unresolved `memory_block` import), then green.
  They pin the §9 copy contract: "has not been sent", `used / budget`,
  reduction target with **no victim named**, **no escape hatch**, composer
  preservation, no automatic retry, no banned vocabulary; and §10's notice
  being generic, change-free, and nominating no candidate.
- `mur-common/src/config.rs` — `cli.seen_permanent_instructions_notice`.

Written but NOT compiled (mur-core is protoc-blocked):
- `memory_cmds.rs` — `instruct_edit` (§7 "Save anyway" via confirmation),
  `PendingKind::EditAnyway { body }`, `set_note_body_on_disk`, plus 5 new
  unit tests (delete confirm + cancel no-op, demote confirm keeps text,
  creation path decides policy, two-section listing, edit rewrites body).
- `app/slash.rs` — `Instruct`, `InstructEdit`, `Pin`, `Unpin` variants +
  parser entries. **This is what made the existing logic reachable at all.**
- `slash_cmds.rs` — dispatch for all four, `settle_memory_outcome`,
  `resolve_memory_confirm`.
- `turn.rs` — confirmation answer consumed before slash parsing; pre-send
  Required-budget gate that blocks with the overlay and leaves the composer
  untouched.
- `app/mod.rs` — `pending_memory_confirm` field.
- `term.rs`, `mod.rs` — one-time migration notice + persistence helpers.
- `complete.rs`, `help_coverage_tests.rs`, `mod.rs` help row — the three
  lists the coverage test ties together.
- `notes_cmd.rs`, `cli/notes.rs`, `dispatch.rs` — `mur notes remove --yes`.

## Two pre-existing build breaks, now fixed

The in-flight work had changed `forget` to return `MemoryOutcome` without
updating either caller; both still formatted it as a `String`. Fixed in
`slash_cmds.rs` and `notes_cmd.rs`. Worth knowing these were already broken
before this session — they are not new breakage.

## Needs verifying when protoc works

Run: `cargo test -p mur-core --lib cmd::agent::cli::memory_cmds` and
`cargo test -p mur-core --lib help_coverage`, then `cargo clippy -p mur-core`.

Specific risks I could not compile away, highest first:

1. **`PendingKind` is no longer `Copy`** (it carries `String`). I changed
   `apply_pending` to `match &op.kind`; any other by-value match will error.
2. **`set_note_body_on_disk` field shape.** Verified against `note_manifest`
   that `content.note` = body while `content.abstract` + `description` = the
   60-char summary. If the note validator has rules about the abstract that I
   did not see, edit could produce an invalid manifest.
3. **Migration notice timing.** Pushed in `term.rs` before the first draw; if
   `push_system` there lands above the welcome banner visually, move it after
   the anchor block. Cosmetic only.
4. **`/instruct-edit <name>` completion** uses `Args::Note`, which offers all
   note names including BestEffort ones. `instruct_edit` rejects a BestEffort
   target with a message pointing at `/pin`, so this is a slightly loose menu
   rather than a wrong action.
5. Acceptance tests 9 and 11 (edit-into-overflow then blocked send; composer
   preservation across a blocked send) are end-to-end through the TUI and have
   no coverage yet — the pieces exist but nothing exercises them together.

## Deliberately not done

- `injector.rs` step 4 — excluded by instruction; not touched (file mtime
  predates this session).
- No escape hatch, no partial injection, no auto-demote anywhere.


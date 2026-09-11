# murmur: `!cmd` output is a turn; shell completion in the composer

**Status:** Approved in conversation 2026-09-11; awaiting plan.
**Scope:** `mur-core/src/cmd/agent/cli/` only. No runtime or protocol change.

## Problem

1. `!cmd` runs locally and its output is *stashed* (`App::pending_shell`) and
   prefixed onto the user's next `message/send`. Two gaps make it look like
   the agent never saw it: a message sent while a turn is streaming goes out
   as `turn/steer`, which does not carry the stash; and nothing on screen says
   the block is waiting. Claude Code's `!` mode, which the user is used to,
   makes the output itself the next user turn and wakes the model at once.
2. The composer has no completion for `!` lines — neither command names nor
   paths — while the slash menu already has the widget, keys and tests.

## Decisions (grilled, in order)

| # | Question | Decision |
|---|---|---|
| 1 | What does "the agent sees the output" mean? | **The output is a turn.** `!cmd` finishes → the `$ cmd` + output block is sent to the agent as the user's message, immediately. |
| 2 | A "run only, don't send" variant (`!!cmd`)? | **No.** One rule. Add later if the cost ever matters. |
| 3 | Completion trigger | **Live menu while typing**, same widget and keys as the slash menu. |

## Design

### 1. `!cmd` as a turn

Flow, replacing the stash:

```
composer "!cmd" ─→ run_local_shell (unchanged: $SHELL -c, timeout, byte cap)
      └→ StreamMsg::ShellDone { cmd, output }
            └→ App::push_shell: one Role::Shell card in the transcript (as today)
            └→ if app.streaming && current_task_id → steer_turn(block)
               else if over_budget → system note "not sent — session budget reached"
               else → start_shell_turn(block)
```

- **`block`** is the text the agent receives:
  `[shell command the user ran locally]\n$ cmd\n<output>\n[end of shell output]`.
  The framing is the existing `take_pending_shell` wording, singular. The
  agent is free to answer with one line; the prompt does not ask for more.
- **`start_shell_turn`** is `start_turn` minus the user bubble: the Shell
  card is the transcript entry, so it must not push a `Role::User` message.
  It persists exactly one channel event for the turn. Today `push_shell`
  persists a `"shell"` turn and `begin_user_turn` a `"user"` turn; the shell
  path keeps the `"shell"` event and does **not** write a second one — the
  agent's reply attaches to it like any turn. Implementation detail for the
  plan: factor the task-id / inflight / spawn half of `begin_user_turn` +
  `start_turn` into a helper both callers use.
- **Steer while streaming:** the same block goes through `turn/steer` (text
  only, which is all a shell block is). No stash survives anywhere:
  `pending_shell`, `take_pending_shell` and their test are deleted.
- **Budget:** `over_budget` refuses a new turn as it does for typed text; the
  Shell card still renders (the command ran), with a system note under it.
- **Removed:** the `[shell commands the user just ran …]` prefix on typed
  messages. A typed message is exactly what the user typed again.

### 2. Shell completion

New module `cli/shell_complete.rs`, pure over inputs it is handed:

```rust
pub struct ShellCompleteCtx<'a> { pub cwd: &'a Path, pub path_bins: &'a [String] }
pub fn candidates(line: &str, ctx: &ShellCompleteCtx) -> Vec<Candidate>
```

- Applies only when the composer line starts with `!` and has no newline.
- Operates on the **last word** (from the last unescaped space to the cursor,
  which is the end of the line in this composer). Other words are untouched.
- **First word** → command names: `ctx.path_bins` filtered by prefix.
  `App` fills `path_bins` once, lazily, on the first `!` completion: every
  executable file in each `$PATH` dir, deduplicated, sorted. Never rescanned
  in-session (a new install needs a new murmur, same as a new shell).
- **Later words** → paths: expand a leading `~`; list the directory of the
  word's parent (relative to `ctx.cwd`), filter by the word's file-name
  prefix; hidden entries only when that prefix starts with `.`; directories
  first, each with a trailing `/`. A directory candidate sets
  `has_children = true`, which the existing accept path already treats as
  "keep the menu open" — so `/`-by-`/` descent is free.
- Cap at `complete::MAX_MENU_ROWS`. `insert` is the whole line with the last
  word replaced (the slash menu's `insert` contract), followed by a space
  for files and commands, nothing for directories.
- Wiring: the two `complete::compute` call sites in `mod.rs` become one
  helper that dispatches on the first character: `/` → `compute`, `!` →
  `shell_complete::candidates`, else `None`. `CompletionState.current` stays
  `None` here — there is no "value in force" for a path.
- Not done: quoting/escaping of spaces in inserted paths (a path with a
  space is inserted as-is; the user quotes it). Named as a ceiling in code.

### 3. Errors

- Shell timeout / non-zero exit / failure to spawn are already inlined into
  the output text by `run_local_shell`; they ride along in the block.
- `$PATH` unset or a dir unreadable → that dir contributes nothing; no error.
- Directory listing failure (permissions) → empty menu, no toast.

### 4. Testing

- `shell_complete` unit tests over a `tempdir` and a literal `path_bins`
  list: first-word prefix filter; path listing with dirs first and `/`;
  hidden-file rule; `~` expansion; last-word-only replacement; cap.
- `mod.rs`: `ShellDone` while idle starts a turn whose outgoing params carry
  the block, pushes **one** Shell card and **no** User bubble, and writes one
  channel event; `ShellDone` while streaming steers with the block; over
  budget → card + note, no dial. Existing `take_pending_shell` test deleted.
- The `/help` guard keeps `!cmd` documented; wording changes to
  "runs a local shell command; its output is sent to the agent as your
  message".

### 5. Docs (after merge, via `update-docs`)

README `!cmd` line, docs-site agent-cli page (`!` and Tab completion rows).

## Out of scope

- `!!` / quiet mode (decision 2).
- Completion for slash-command *arguments* that are paths (`/skill add <path>`).
- Escaping spaces in inserted paths.

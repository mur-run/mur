# murmur: `!cmd` streams and is cancelled, never killed by a clock

**Status:** Approved in conversation 2026-09-13; awaiting plan.
**Scope:** `mur-core/src/cmd/agent/cli/` only (`stream.rs`, `app.rs`, `mod.rs`, `ui/message.rs`). No runtime, protocol, or agent-side change.
**Issue:** #1286. Sibling of the agent-side fix in #1288 (`docs/superpowers/specs/2026-09-12-bash-yield-not-kill-design.md`), found by that spec's §1 scan.

## 1. Problem

`!cargo test` in murmur dies at 30 seconds with `[timed out after 30s]`:

```rust
// mur-core/src/cmd/agent/cli/stream.rs
pub const SHELL_TIMEOUT_SECS: u64 = 30;
…
match tokio::time::timeout(Duration::from_secs(SHELL_TIMEOUT_SECS), fut).await {
    Err(_) => return format!("[timed out after {SHELL_TIMEOUT_SECS}s]"),
```

Three things are wrong, and only the first is the one in the issue title:

1. **A clock kills work a human is watching.** The execution-limits spec's D3
   says an attended run has no hard stops — the human is the guard. `!cmd` is
   the most attended thing in the product: the user typed it a second ago and
   is looking at the screen. A 30 s ceiling is the agent-side `bash` bug
   (#1288) in a different file.
2. **Nothing is visible until it ends.** `Command::output()` buffers both
   pipes to completion, so a three-minute test run shows one line —
   ``running `cargo test`…`` — and then everything at once. Even with the
   timeout gone, the user cannot tell a slow command from a wedged one.
3. **The kill is not a kill.** The child is spawned with `kill_on_drop(true)`
   and no process group, so killing the shell leaves `cargo`'s `rustc`
   children running. Exactly the D9 orphan the agent-side fix addressed;
   here it has been latent because nothing but the 30 s clock ever killed
   anything.

### 1.4 Found while reading: a failing command can reach the agent looking clean

Not in the issue, fixed here because the fix is one word. The cap runs
**after** the exit marker is appended, and keeps the head:

```rust
text.push_str(&format!("[exit {}]", out.status.code().unwrap_or(-1)));   // last
…
text.truncate(cut);                 // keeps the FIRST 8 KiB
text.push_str("\n[output truncated]");
```

So any command whose output exceeds 8 KiB loses its `[exit N]` line to the
truncation. `!cargo test` with a real test suite is over 8 KiB routinely, so
today a failing run is forwarded to the agent as `Compiling …` with no exit
marker and no failure summary — both live at the end. The agent is told a
failure looks like a success, which is the one thing a settlement-card
codebase should not do.

Keeping the tail instead fixes this for free: the exit marker and the
`test result: FAILED` line are the last things written, so they are the last
things dropped. `[output truncated]` moves to the front, where it describes
what is missing.

## 2. Decisions

| # | Decision | Rejected alternative |
|---|---|---|
| D1 | **No wall-clock limit.** `SHELL_TIMEOUT_SECS` is deleted, not raised. Ctrl-C is the only bound. | Raise it to 10 minutes — the next `cargo build --release` hits it, and the number would be a guess about someone else's machine. |
| D2 | **Output streams into the transcript as it arrives.** Both pipes are read incrementally and each chunk appends to a live `Role::Shell` card. | Keep `output()` and only drop the timeout — leaves the user staring at one static line for minutes with no way to tell slow from hung. |
| D3 | **Ctrl-C kills the shell first, and only the shell.** When a `!cmd` is running, Ctrl-C ends it and leaves any in-flight agent turn alone; the turn still has its own `Esc`-`Esc`. With no shell running, Ctrl-C behaves exactly as today. | Cancel the turn first (today's rule) — the shell is the thing the user just launched and is watching, and it would be unkillable while a turn streams. Kill both — unrecoverable when the user meant one. |
| D4 | **A cancelled command does not wake the agent.** Its card stays in the transcript (it really ran), but no turn starts and no steer is sent. | Send the partial output with a `[cancelled]` marker — Ctrl-C means "never mind", and waking the model with half a test run is the opposite of what the keystroke said. |
| D5 | **The child is its own process group; the kill signals the group.** `process_group(0)` at spawn; `SIGTERM` to the group, then `SIGKILL` after a 2 s grace. | Kill the direct shell only — `rustc`/test binaries survive and keep the CPU, which is the #1288 D9 regression in a new place. |
| D6 | **Two independent caps, both keeping the tail.** The transcript card keeps the last `SHELL_CARD_MAX_BYTES` (256 KiB); the block forwarded to the agent keeps the last `SHELL_MAX_BYTES` (8 KiB — the value is unchanged, the end kept is not; see §1.4). | One shared cap — the card wants scrollback for a human, the block wants to stay cheap in tokens; one number cannot serve both. No card cap — `!yes` grows the TUI's memory without bound. Keep the head, as today — for a test run the verdict is the last line, and see §1.4. |

## 3. Design

### 3.1 Flow

```
composer "!cmd"
   └→ spawn child ($SHELL -c, process_group(0), stdout+stderr piped)
      │  cancel: oneshot::Receiver held by the task
      │  handle: oneshot::Sender parked in App::shell_cancel
      │
      ├→ per read      → StreamMsg::ShellOutput { chunk }
      │                    └→ App::append_shell_output — live Role::Shell card
      │
      ├→ Ctrl-C        → App::shell_cancel.take().send(())
      │                    └→ killpg(SIGTERM) → 2 s → killpg(SIGKILL)
      │
      └→ child exits   → StreamMsg::ShellDone { cmd, exit, cancelled }
                           └→ finalise card (streaming = false, persist)
                           └→ route_shell_output(…, cancelled) →
                                Start | Steer | Skip(reason)
```

### 3.2 `stream.rs`

`run_local_shell(cmd) -> String` is replaced by:

```rust
/// Run a local `!command`, streaming its output to the UI. Returns when the
/// child exits or the cancel signal fires. Never bounded by a clock (D1).
pub async fn run_local_shell_streaming(
    cmd: String,
    tx: mpsc::Sender<StreamMsg>,
    cancel: oneshot::Receiver<()>,
) -> ShellOutcome
```

- Spawn: `$SHELL -c` / `cmd /C` as today, plus `.stdout(piped()).stderr(piped())`
  and, on unix, `.process_group(0)` (D5). `kill_on_drop(true)` stays as the belt.
- Pump: one task per pipe, both feeding a `tokio::sync::mpsc` of `Vec<u8>`;
  the main select loop forwards each as `StreamMsg::ShellOutput { chunk }`.
  stdout and stderr interleave in arrival order, which is what a terminal
  shows and what the old code approximated by concatenating them.
- Chunks are decoded with `String::from_utf8_lossy` **per chunk**, which can
  split a multi-byte character across a read boundary. A read boundary is
  chosen by the kernel, so this is real. The pump keeps an incomplete
  trailing UTF-8 sequence (≤ 3 bytes) back until the next read, the same
  carry the runtime's `StreamMasker` uses; at EOF whatever is left is decoded
  lossily.
- Cancel: `tokio::select!` on the cancel receiver. On fire, `killpg(pid,
  SIGTERM)`, wait up to `KILL_GRACE` (2 s) for the child, then `killpg(pid,
  SIGKILL)`. Keep pumping the pipes through the grace window so the last
  output before the signal still reaches the card.
- Returns `ShellOutcome { exit: Option<i32>, cancelled: bool }`. The text
  lives in the card (§3.3), not here — one accumulation, not two.

`signal_group` is a ~10-line local helper in this file, deliberately **not**
shared with `mur-agent-runtime::tools::bash_jobs` even though `mur-core`
depends on that crate. The shared part is three libc calls; the surrounding
shapes differ (a job table and a watch channel there, one child and a oneshot
here), and making the runtime's private helper public would couple the TUI's
local shell-out to the agent's tool-execution internals. The comment on the
helper names #1288 D9 as the reason the group exists at all.

### 3.3 `app.rs`

One new field and one new method:

```rust
/// Cancel handle for the `!cmd` in flight, if any. `Some` means a local
/// shell command is running, which is what makes Ctrl-C kill it rather
/// than the agent turn (D3).
pub shell_cancel: Option<tokio::sync::oneshot::Sender<()>>,
```

```rust
/// Append streamed `!cmd` output to the live shell card, creating it on the
/// first chunk. Head-drops past SHELL_CARD_MAX_BYTES so a chatty command
/// cannot grow the transcript without bound (D6).
pub fn append_shell_output(&mut self, chunk: &str)
```

`push_shell` keeps its name and its job — but is now the *finaliser*: it
marks the live card `streaming = false`, appends `[exit N]` when non-zero,
and persists. The persisted text is the card's text, capped as the card is.
A command that produced no output still gets its `$ cmd` card, as today.

Mirrors `append_delta`, deliberately: a streaming `ChatMsg` already skips the
`rendered` markdown cache and already re-renders every frame, and
`append_delta`'s comment about not resetting `scroll_back` applies verbatim
here — a user scrolled up to read earlier output must not be yanked to the
bottom by each new line.

### 3.4 `mod.rs`

- `submit`: build the oneshot pair, park the sender in `app.shell_cancel`,
  pass the receiver to the task. The ``running `cmd`…`` system line is
  **removed** — the live card with its own "running" footer replaces it, and
  two indicators for one command is one too many.
- `handle_ctrl_c`: new first branch (D3).

  ```rust
  if let Some(cancel) = app.shell_cancel.take() {
      let _ = cancel.send(());          // receiver dropped = child already gone
      return;                           // the turn, if any, is untouched
  }
  ```

  No system note here: the card's footer changes to `[cancelled]` when
  `ShellDone` lands, which is the same information in the place the user is
  already looking.
- `handle_stream`: `ShellOutput` → `app.append_shell_output(&chunk)`.
  `ShellDone` → clear `app.shell_cancel`, finalise the card, then route.
- `route_shell_output` gains a leading `cancelled: bool` parameter and a new
  first arm returning `ShellRoute::Skip("cancelled — not sent to the agent")`
  (D4). It stays a pure function with its existing unit tests; the new case
  gets one more.

### 3.5 `ui/message.rs`

The `Role::Shell` arm gains a footer line while `m.streaming`:

```
$ cargo test
   Compiling mur-core v2.80.0
    Finished test profile in 1m 12s
⠋ running · Ctrl-C to stop
```

Same spinner frame the agent header already uses, so the two live states
animate together. When the card is finalised the footer is dropped; a
cancelled card ends with a dim `[cancelled]` line instead.

## 4. Error handling

| Situation | Behaviour |
|---|---|
| spawn fails | `[failed to run: <e>]` as a one-shot card, as today |
| `process_group(0)` refused | spawn fails and says so; never a silent ungrouped child, which would make Ctrl-C lie |
| non-zero exit | `[exit N]` appended on finalise, as today; still routed to the agent (a failing test run is exactly what the agent should see) |
| child ignores SIGTERM | SIGKILL to the group after `KILL_GRACE`; the card still finalises |
| Ctrl-C pressed twice | the second finds `shell_cancel` already `None` and falls through to today's behaviour — it does not double-signal |
| command exits between the keypress and the send | `oneshot::send` returns `Err` (receiver dropped); ignored, the natural `ShellDone` wins |
| output is not UTF-8 | lossy per chunk, with the ≤3-byte carry described in §3.2; never panics, never splits a character across two cards |

## 5. Testing

Unit, in `stream.rs`, `app.rs` and `mod.rs`:

1. **No clock:** a command that runs longer than the old 30 s ceiling
   completes normally. Uses a 1.5 s sleep against an asserted absence of any
   timeout path — the constant is gone, so the real assertion is that
   `SHELL_TIMEOUT_SECS` no longer exists and the command's own exit is what
   ends the call.
2. **Streaming:** a command printing three lines with a pause between them
   produces more than one `ShellOutput` before `ShellDone` — the assertion
   that separates streaming from buffering.
3. **Cancel kills the group (D5):** run `sleep 60 & echo $!; wait`, capture
   the grandchild pid from the streamed output, fire the cancel, assert the
   grandchild is dead within `KILL_GRACE + 1 s`. The direct-child-only
   regression this exists for.
4. **Cancel is prompt:** the call returns within the grace window, not after
   the command's natural duration.
5. **Cancelled output is not sent (D4):** `route_shell_output(cancelled =
   true, …)` is `Skip`, for every combination of streaming/idle and
   over/under budget — cancelled outranks all of them.
6. **Ctrl-C precedence (D3):** with `shell_cancel` set and `streaming` true,
   `handle_ctrl_c` takes the cancel handle and leaves `current_task_id` and
   `streaming` untouched; with `shell_cancel` `None`, the existing
   cancel-the-turn behaviour is unchanged (the current tests keep passing).
7. **Card cap (D6):** appending past `SHELL_CARD_MAX_BYTES` keeps the tail,
   drops the head, and leaves the `$ cmd` first line intact.
8. **Agent block cap:** the forwarded block is still ≤ `SHELL_MAX_BYTES`
   even when the card holds far more, and it keeps the **tail** — a
   >8 KiB output whose last line is `[exit 1]` forwards that line, which is
   the §1.4 regression. `[output truncated]` leads the block.
9. **UTF-8 across a read boundary:** feeding a multi-byte character split
   across two chunks yields the character once, not two replacement chars.
10. **Finalise:** a non-zero exit appends `[exit N]`; a zero exit does not;
    an empty-output command still leaves exactly one `$ cmd` card.

Live check (not CI): `!sleep 45` completes rather than dying at 30 s;
`!cargo build` streams progress and Ctrl-C stops it with `pgrep` showing no
surviving `rustc`; Ctrl-C during a `!cmd` that runs alongside a streaming
turn leaves the turn running.

Two existing tests in `stream.rs` change with the behaviour, and the plan
must update rather than delete them:
`run_local_shell_captures_output_and_exit` (same assertions, new streaming
entry point) and `run_local_shell_truncates_huge_output` (still capped, but
now `starts_with("[output truncated]")` instead of `ends_with`, per §1.4).

## 6. Out of scope

- Interactive `!cmd` (a command that reads stdin). stdin stays `null`, as
  today. A TUI already owns the terminal; handing it to a child is its own
  design.
- A background variant (`!!cmd`, or a job table like the agent-side one).
  The agent's `bash` needs handles because a model cannot watch a screen; a
  human can, and the 2026-09-11 bang-turn spec already decided against a
  second bang form.
- Re-running the previous `!cmd` on a key. Unrelated, and the composer's
  history already recalls the line.

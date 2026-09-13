# murmur: `!cmd` streams and is cancelled, never killed by a clock

**Status:** Approved in conversation 2026-09-13; revised the same day after review (seven findings, each checked against the source before the design moved — see §7). Awaiting plan.
**Scope:** `mur-core/src/cmd/agent/cli/` only — a new `shell.rs` submodule plus thin edits to `stream.rs`, `app.rs`, `mod.rs`, `ui/message.rs`. No runtime, protocol, or agent-side change.
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
| D2 | **Output streams into the transcript as it arrives.** Both pipes are read incrementally and each chunk appends to a live `Role::Shell` card, which is created **when the command is accepted**, not on first output, so a silent command is still visibly running. | Keep `output()` and only drop the timeout — leaves the user staring at one static line for minutes with no way to tell slow from hung. |
| D3 | **Ctrl-C kills the shell first, and only the shell.** When a `!cmd` is running, Ctrl-C ends it and leaves any in-flight agent turn alone; the turn still has its own `Esc`-`Esc`. With no shell running, Ctrl-C behaves exactly as today. | Cancel the turn first (today's rule) — the shell is the thing the user just launched and is watching, and it would be unkillable while a turn streams. Kill both — unrecoverable when the user meant one. |
| D4 | **A cancelled command does not wake the agent.** Its card stays in the transcript (it really ran), but no turn starts and no steer is sent. | Send the partial output with a `[cancelled]` marker — Ctrl-C means "never mind", and waking the model with half a test run is the opposite of what the keystroke said. |
| D5 | **The child is its own process group; the kill signals the group.** `process_group(0)` at spawn; `SIGTERM` to the group, then `SIGKILL` after a 2 s grace. Windows is a documented weaker case (§3.7). | Kill the direct shell only — `rustc`/test binaries survive and keep the CPU, which is the #1288 D9 regression in a new place. |
| D6 | **Two independent caps, both keeping the tail.** The transcript card keeps the last `SHELL_CARD_MAX_BYTES` (256 KiB); the block forwarded to the agent keeps the last `SHELL_MAX_BYTES` (8 KiB — the value is unchanged, the end kept is not; see §1.4). | One shared cap — the card wants scrollback for a human, the block wants to stay cheap in tokens; one number cannot serve both. No card cap — `!yes` grows the TUI's memory without bound. Keep the head, as today — for a test run the verdict is the last line, and see §1.4. |
| D7 | **`!cmd` is single-flight.** A second `!cmd` while one runs is refused with a note naming Ctrl-C. | Concurrent runs with a `shell_id` on every event and one card per run — real complexity (N cards, N handles, N routing decisions) for something nobody asked for, in a composer where a human types one line at a time. A shell gives you one foreground job; so does this. |
| D8 | **One teardown path, reached from every exit.** `cancel_shell` is called by Ctrl-C, quit, `/clear` and a channel switch; every event carries the generation it belongs to, and the UI drops events from a generation that is no longer current. | Wire cancellation to Ctrl-C alone — `/clear` would wipe the card while the process ran on invisibly, and the stale `ShellDone` would land in whatever conversation is open by then (§7, finding 2). |
| D9 | **Every chunk the command will ever produce is forwarded before `ShellDone` is sent.** The engine drains its pipes after the child exits and only then returns; the caller sends `ShellDone` after that, on the same ordered channel. When a grandchild holds the pipes open past the drain grace, the card says so rather than silently truncating. | Send `ShellDone` on child exit — child exit and pipe EOF are different events, so the card would finalise, persist and route while output was still in flight, and the late chunks would be dropped by a finalised card. |
| D10 | **How a command ended is one enum, not two loose fields.** `ShellEnd::{Exited, Signaled, Cancelled, SpawnFailed}`. | `{ exit: Option<i32>, cancelled: bool }` — `exit: None` cannot distinguish "killed by a signal" from "spawn failed" from "no code reported", and the spawn error message has nowhere to live. |

## 3. Design

### 3.1 A new `cli/shell.rs`

Everything about running a local command moves into one module. `stream.rs`
keeps its job — the A2A streaming bridge — and gains only the two new
`StreamMsg` variants, because the enum lives there.

This is also what the repository's own size rule requires: `app.rs` is 2894
lines and `mod.rs` is 3770, both already far past the 800-line limit in
CLAUDE.md §4, and adding a shell lifecycle to either would deepen the
violation. The rule's remedy is a submodule, movement first: the existing
`shell_block` and `route_shell_output` move out of `mod.rs` unchanged, as
their own commit, before anything new is written.

`shell.rs` owns: the spawn, the pumps, the kill, the caps, `ShellEnd`,
`ShellState`, `shell_block` and `route_shell_output`. `App` holds one
`ShellState`; `mod.rs` keeps only the four call sites that reach it.

### 3.2 Flow

```
composer "!cmd"
   ├─ shell already running? → refuse with a note (D7); nothing else happens
   │
   ├─ spawn child ($SHELL -c, process_group(0), stdout+stderr piped)
   │     synchronously in `submit`, so the pid is known to the UI at once —
   │     which is what lets quit signal the group without waiting (§3.5)
   │
   ├─ App::shell = ShellState { gen: n, pid, cancel: Sender }
   ├─ App::begin_shell(cmd)  → live Role::Shell card, immediately (D2)
   │
   └─ task: run(child, tx, cancel_rx, gen)
         ├→ per read        → StreamMsg::ShellOutput { gen, chunk }
         ├→ cancel fires    → killpg(SIGTERM) → 2 s → killpg(SIGKILL)
         ├→ child exits     → drain both pipes (bounded), forwarding
         └→ then, and only then → StreamMsg::ShellDone { gen, cmd, end }   (D9)

UI, for either event: gen != App::shell.gen → drop it (D8).
```

### 3.3 `ShellEnd` (D10)

```rust
pub enum ShellEnd {
    /// Ran to completion with this status code.
    Exited(i32),
    /// A signal ended it. `Some` where the platform reports the number.
    Signaled(Option<i32>),
    /// The user pressed Ctrl-C, or a teardown path called `cancel_shell`.
    Cancelled,
    /// The shell could not be started at all.
    SpawnFailed(String),
}
```

What each writes on the card, and whether it reaches the agent:

| End | Card tail | To the agent? |
|---|---|---|
| `Exited(0)` | nothing | yes |
| `Exited(n)` | `[exit n]` | yes |
| `Signaled(Some(9))` | `[killed by signal 9]` | yes |
| `Signaled(None)` | `[killed by a signal]` | yes |
| `Cancelled` | `[cancelled]` | **no** (D4) |
| `SpawnFailed(e)` | `[failed to run: e]` | yes — "command not found" is exactly what the agent should hear |

### 3.4 Streaming and the UTF-8 carry

One task per pipe, each with its own carry buffer, both feeding one channel.
Per-pipe carries, not one shared: a read boundary is chosen by the kernel and
can split a multi-byte character, and interleaving two streams' bytes through
a single carry would corrupt both. Each pump holds an incomplete trailing
sequence (≤ 3 bytes) back until its next read rather than emitting a
replacement character for a character that is perfectly fine. At EOF whatever
remains is decoded lossily. A carry that cannot be completed — genuinely
non-UTF-8 output — is flushed lossily rather than grown without bound.

### 3.5 Cancellation and teardown (D8)

```rust
/// End the running `!cmd`, if any, and retire its generation so nothing it
/// still emits reaches the transcript. Every exit from the current
/// conversation calls this — not just Ctrl-C.
pub fn cancel_shell(app: &mut App, hard: bool)
```

- `hard = false` (Ctrl-C): send on the cancel channel; the task does
  SIGTERM → grace → SIGKILL and finishes reporting.
- `hard = true` (quit): the UI is about to stop reading, so there is no one
  left to run the escalation. Signal the group directly and synchronously —
  `SIGTERM` then `SIGKILL` — using the pid `submit` recorded. This is the
  finding that the old design got wrong: dropping the task on quit fires
  `kill_on_drop`, which kills the direct shell and leaves the group, i.e.
  exactly the orphan D5 exists to prevent.
- Both bump `App::shell.gen`, so anything the dying task still emits is
  dropped by the UI rather than landing in a cleared transcript or a
  different channel.

Call sites, all four: `handle_ctrl_c` (soft), `request_quit` (hard),
`SlashCmd::Clear` before `start_new_session` (soft), and `switch_channel`
(soft).

### 3.6 Rendering

The `Role::Shell` arm gains a footer line while `m.streaming`:

```
$ cargo test
   Compiling mur-core v2.80.0
    Finished test profile in 1m 12s
⠋ running · Ctrl-C to stop
```

The spinner behind that frame is driven by the event loop's ticker, which is
today gated on `app.streaming` — an agent turn. A shell-only command does not
set that flag, so the guard becomes `app.streaming || app.shell.is_running()`;
without it the footer would render once and freeze, which reads as hung.

### 3.7 Windows

`process_group(0)` and `killpg` are unix. On Windows the kill is
`taskkill /F /T /PID`, a tree kill: it reaches children, but unlike a Job
Object it does not bind a process that deliberately detaches. D5's guarantee
is therefore **unix-strength on unix and best-effort on Windows**, the same
boundary the agent-side spec drew, and Job Objects stay out of scope until a
Windows user reports an orphan this misses.

## 4. Error handling

| Situation | Behaviour |
|---|---|
| spawn fails | `ShellEnd::SpawnFailed(e)`; the card shows `[failed to run: e]` and the block still reaches the agent |
| `process_group(0)` refused | the spawn fails and says so; never a silent ungrouped child, which would make every later kill lie |
| second `!cmd` while one runs | refused before anything is spawned, with `` a `!command` is already running — Ctrl-C to stop it `` (D7) |
| child ignores SIGTERM | SIGKILL to the group after `KILL_GRACE`; the card still finalises |
| Ctrl-C pressed twice | the second finds no shell state and falls through to today's behaviour — it does not double-signal |
| command exits between the keypress and the send | the cancel send returns `Err` (receiver dropped); ignored, the natural end wins |
| grandchild holds the pipes past the drain grace | the card gains `[output may be incomplete — a background process still holds this command's pipes]`; never an unbounded wait (D9) |
| output is not UTF-8 | lossy per chunk, with the ≤3-byte carry of §3.4; never panics, never splits a character across two cards |
| events from a retired generation | dropped by the UI; they cannot write into a cleared transcript or another channel (D8) |

## 5. Testing

Unit, in `shell.rs`, `app.rs` and `mod.rs`:

1. **No clock (D1):** a command outliving the old 30 s ceiling finishes on
   its own terms. `SHELL_TIMEOUT_SECS` no longer exists.
2. **Streaming (D2):** a command printing, pausing, then printing again
   yields a chunk *before* the command ends — the assertion that separates
   streaming from buffering.
3. **Cancel kills the group (D5):** `sleep 60 & echo $!; wait` — capture the
   grandchild pid from the streamed output, cancel, assert the grandchild is
   dead within `KILL_GRACE + 1 s`.
4. **Cancel is prompt:** the call returns within the grace window, not after
   the command's natural duration.
5. **Cancelled output is not sent (D4):** `route_shell_output` returns `Skip`
   for a cancelled run in every combination of streaming/idle and
   over/under budget — cancelled outranks all of them.
6. **Ctrl-C precedence (D3):** with a shell running and a turn streaming,
   `handle_ctrl_c` ends the shell and leaves `current_task_id` and
   `streaming` untouched; with no shell, the existing behaviour and its
   existing tests are unchanged.
7. **Card cap (D6):** appending past `SHELL_CARD_MAX_BYTES` keeps the tail,
   drops the head, and leaves the `$ cmd` first line intact.
8. **Agent block cap (§1.4):** the forwarded block is ≤ `SHELL_MAX_BYTES`,
   keeps the **tail** so a trailing `[exit 1]` survives, and leads with
   `[output truncated]`.
9. **UTF-8 across a read boundary:** a multi-byte character split across two
   reads yields the character once, not two replacement characters.
10. **Ends (D10):** each `ShellEnd` variant writes its documented card tail,
    and only `Cancelled` is withheld from the agent.
11. **Single-flight (D7):** submitting a second `!cmd` while one runs leaves
    exactly one live card, does not replace the cancel handle, and pushes the
    refusal note. The regression this exists for: overwriting the handle
    drops the first `oneshot::Sender`, which resolves its receiver as
    cancelled and would silently kill the first command.
12. **Teardown (D8), one test per path:** Ctrl-C, quit, `/clear` and a
    channel switch each cancel the group and retire the generation; a
    `ShellOutput` or `ShellDone` arriving afterwards is dropped — no card, no
    persisted turn, no system note in the new conversation.
13. **Quit is hard (§3.5):** the quit path signals the group directly rather
    than relying on the escalation task, which the shutting-down runtime
    would never run.
14. **Ordering (D9):** the last `ShellOutput` always precedes `ShellDone` in
    the channel, for a command that writes immediately before exiting.
15. **Silent command (D2):** `!sleep 2` shows a card with the running footer
    within a frame of being submitted, before any output exists.
16. **Spinner (§3.6):** the tick guard admits a shell-only command, so the
    footer animates when no agent turn is live.

Live check (not CI): `!sleep 45` completes rather than dying at 30 s;
`!cargo build` streams progress and Ctrl-C stops it with `pgrep` showing no
surviving `rustc`; Ctrl-C during a `!cmd` alongside a streaming turn leaves
the turn running; `/clear` mid-command leaves no `rustc` behind and no stray
line in the new conversation; quitting mid-command leaves no `rustc` behind.

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
  second bang form. D7 makes this explicit: one foreground job.
- Windows Job Objects (§3.7).
- Re-running the previous `!cmd` on a key. Unrelated, and the composer's
  history already recalls the line.

## 7. Review log

2026-09-13, seven findings, each checked against the source before the
design moved:

| Finding | Verified at | Resolution |
|---|---|---|
| a second `!cmd` corrupts the first | `submit`'s bang arm has no guard; one unscoped `shell_cancel`; `streaming_shell_mut` takes the newest card | **Worse than reported and now D7.** Overwriting the field *drops* the first `oneshot::Sender`, and a dropped sender resolves its receiver — so starting a second command would have silently cancelled the first, not merely tangled their output. |
| shell work escapes a teardown | `start_new_session` (`app.rs:1336`) replaces `self.session` and clears `messages` without touching shell state; `persist_turn` writes to `self.session`; `request_quit` (`mod.rs:1652`) only sets `should_quit` | **Confirmed, now D8 + §3.5.** On quit the runtime drops the task and `kill_on_drop` reaches the direct shell only — the exact orphan D5 exists to prevent. |
| `ShellDone` can overtake trailing output | the plan already drained before returning and the channel is FIFO, so the ordering itself held | **Partly confirmed, now D9.** The ordering claim was already satisfied; the real hole was the bounded drain discarding late output *silently*. The bound stays (a grandchild can hold the pipes forever) and now reports itself. |
| a silent command shows nothing | the plan already created the card in `submit`; the spec's §3.3 wording said "on the first chunk" | **Spec defect, now D2.** The plan was right and the spec was not — which is its own bug, since the spec is what gets read later. |
| the spinner freezes for a shell-only command | `mod.rs:1039` — `_ = spinner.tick(), if app.streaming` | **Confirmed, new, now §3.6.** No agent turn means no tick, so the footer would have rendered once and frozen. |
| D5 undefined on Windows | the plan had a `#[cfg(not(unix))]` `taskkill /T`; the spec was silent | **Spec defect, now §3.7**, stated as a weaker guarantee rather than an unqualified promise. |
| the edits deepen oversized modules | `app.rs` 2894, `mod.rs` 3770 vs CLAUDE.md §4's 800 | **Confirmed, now §3.1.** New `cli/shell.rs`; the two existing helpers move there first as pure movement, in their own commit. |

# Plan: murmur `!cmd` streams and is cancelled, never killed by a clock

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-13-murmur-bang-streaming-design.md` (D1–D10, §3.1–§3.7, §7 review log).
> Issue: #1286. Base: `origin/main` at or after `a78427e5`.
>
> **Two PRs, in order.** PR 1 is Task 0 alone — pure code movement, which CLAUDE.md §4 requires to be its own PR, not merely its own commit. Merge it, then rebase PR 2 (Tasks 1–3) onto the new `main`. Branches: `refactor/cli-shell-module` then `feat/murmur-bang-streaming`, both in a worktree under `.worktrees/`.
>
> **Every commit builds.** Tasks 1–3 change an enum, its producer and its consumers together; splitting them would leave commits that cannot compile, so they are three *phases of one task* with a single commit and a single clippy run at the end. Do not commit mid-cutover.

**Goal.** A `!cmd` in murmur runs with no wall-clock limit, streams its output into the transcript from the moment it is accepted, and ends only when it finishes or when the user stops it — by Ctrl-C, by quitting, by `/clear`, or by switching channel — at which point its whole process group dies and nothing it still emits reaches the screen.

**Architecture.** All local-command logic lives in a new `cli/shell.rs`: spawn, pipe pumps, process-group kill, caps, `ShellEnd`, and the `ShellState` that makes `!cmd` single-flight and tags every event with a generation. `stream.rs` keeps its job (the A2A streaming bridge) and gains only the two new `StreamMsg` variants, because the enum lives there. `App` holds one `ShellState`; `mod.rs` keeps four thin call sites.

**Tech stack.** Rust 2024, `cargo nextest`. `mur-core` env: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`. `libc` is already a `mur-core` dependency (`mur-core/Cargo.toml:42`) — do not add it.

## Global Constraints (from the spec)

- **D1:** delete `SHELL_TIMEOUT_SECS`. Do not raise it, do not replace it with a larger constant, do not add a configurable one. The user is the only bound.
- **D2:** the card is created when the command is **accepted**, not on first output, and each read forwards a chunk before the command ends.
- **D3:** Ctrl-C ends the shell and **only** the shell. With no `!cmd` running, `handle_ctrl_c` behaves exactly as today and its existing tests must keep passing unchanged.
- **D4:** a cancelled command's output is never sent to the agent — no turn, no steer. Its card stays in the transcript.
- **D5:** unix children are spawned with `process_group(0)`; the kill is `killpg(SIGTERM)` then `killpg(SIGKILL)` after `KILL_GRACE` (2 s). A `process_group` failure fails the spawn — never fall back to an ungrouped child. Windows is best-effort `taskkill /F /T` (§3.7), and the code says so.
- **D6:** two caps, both keeping the **tail**: the card at `SHELL_CARD_MAX_BYTES` (256 KiB), the agent block at `SHELL_MAX_BYTES` (8 KiB, value unchanged).
- **D7:** `!cmd` is single-flight. A second one is refused **before anything is spawned**. Never overwrite a live cancel handle — dropping an `oneshot::Sender` resolves its receiver, which would silently cancel the first command.
- **D8:** one teardown path. `shell::cancel` is called by Ctrl-C, quit, `/clear` and a channel switch; every event carries its generation and the UI drops events from a retired one.
- **D9:** every chunk a command will ever produce is forwarded before `ShellDone`.
- **D11:** a cancelled card is finalised synchronously by `stop_shell`, at the keypress. Never rely on the late `ShellDone` to stamp it — that event carries a generation the same call has just retired, so it is dropped by design.
- **§3.5:** a soft cancel keeps the pid in `Cancelling` until the task reports back; `ShellDone` calls `state.done(gen)` **unconditionally**, before the UI's generation check. The SIGKILL escalation lives inside `run`'s select loop, never in a detached task. When a grandchild holds the pipes past the drain grace, the card says so rather than truncating silently.
- **D10:** how a command ended is one enum, `ShellEnd`, not two loose fields.
- **§1.4:** the agent block keeps the tail so `[exit N]` and a failure summary survive; `[output truncated]` leads the block.
- **§3.1 / CLAUDE.md §4:** `app.rs` is 2894 lines and `mod.rs` is 3770, both already past the 800-line rule. New code goes in `cli/shell.rs`. Task 0 is pure movement in its own commit, as the rule requires.
- stdin stays `Stdio::null()`. Interactive `!cmd` is out of scope.
- `mur-core` must not reach into `mur-agent-runtime::tools::bash_jobs` for the kill helper even though the dependency exists — `signal_group` is local to `shell.rs` (spec §3.1 records why).
- Before every commit: `cargo fmt --all`, then `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c.log 2>&1; echo $?` — read the **exit code**, never a grep of the output. Every commit this plan asks for must satisfy that; no commit may be made from a tree that does not build.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-core/src/cmd/agent/cli/shell.rs` (new) | everything about running a local command: `shell_block`, `route_shell_output`, caps, `cap_tail`, `signal_group`, `ShellEnd`, `spawn`, `run`, `pump`, `ShellState`, `cancel` | 0, 1, 2 |
| `mur-core/src/cmd/agent/cli/mod.rs` | `mod shell;`; the four call sites; submit's single-flight guard; the spinner guard | 0, 3 |
| `mur-core/src/cmd/agent/cli/stream.rs` | the two new `StreamMsg` variants; deletion of the old runner and its constants | 1 |
| `mur-core/src/cmd/agent/cli/app.rs` | the `ShellState` field and the three card operations | 2 |
| `mur-core/src/cmd/agent/cli/ui/message.rs` | the running footer on a live shell card | 4 |

---

## Task 0 — Pure movement: `shell_block` and `route_shell_output` into a new `shell.rs`

No behaviour change. **Its own PR on `refactor/cli-shell-module`**, because CLAUDE.md §4 asks for pure movement in a separate PR — a reviewer confirms it with `git show --stat` and a diff that only moves lines. Merge it before starting Task 1, then branch `feat/murmur-bang-streaming` from the new `main`.

**Interfaces.** Produces: module `crate::cmd::agent::cli::shell` exporting `shell_block`, `ShellRoute`, `route_shell_output`. Consumes: nothing.

- [x] Create `mur-core/src/cmd/agent/cli/shell.rs` containing **only** this header and the three items cut verbatim from `mod.rs` (`shell_block`, `enum ShellRoute`, `route_shell_output`, with their doc comments unchanged). `ShellRoute` and `route_shell_output` were private to `mod.rs`; they become `pub(super)`:

```rust
//! Local `!command` execution for the murmur TUI: spawning, streaming,
//! cancelling, and deciding where a finished command's output goes.
//!
//! Separate from `stream.rs`, which is the A2A streaming bridge and has
//! nothing to do with local processes, and separate from `mod.rs`, which is
//! already far past the repository's 800-line rule (CLAUDE.md §4).
```

- [x] Delete those three items from `mod.rs` and add `mod shell;` beside its other `mod` declarations. Add `use shell::{ShellRoute, route_shell_output, shell_block};` — or qualify at the two call sites, whichever leaves `mod.rs` smaller.
- [x] Move the three tests that cover them (`grep -n "shell_block\|route_shell_output" mur-core/src/cmd/agent/cli/mod.rs` inside the test module) into a `#[cfg(test)] mod tests` in `shell.rs`, unchanged apart from the `use super::*;` they need.
- [x] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli:: > /tmp/t0.log 2>&1; echo $?` → `0`. Same test count as before the move; nothing was rewritten.
- [x] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c0.log 2>&1; echo $?` → `0`.
- [x] Commit: `refactor(murmur): move shell_block/route_shell_output into cli/shell.rs (no behaviour change) (#1286 T0)`.
- [x] Open PR 1, title `refactor(murmur): move shell helpers into cli/shell.rs (no behaviour change)`, body naming CLAUDE.md §4 and stating that the diff only moves lines. Merge it on green CI.
- [x] `git fetch origin && git checkout -b feat/murmur-bang-streaming origin/main` — Tasks 1–4 build on the merged movement, not on top of the unmerged branch.

---

## Task 1 — the cutover (three phases, ONE commit)

Phases A–C below change `StreamMsg`, its producer and its consumers. Rust
will not compile any intermediate state, so there is **one** commit, at the
end of phase C, and **one** clippy run. Run `cargo check -p mur-core` between
phases if you want a progress signal — expect errors until C closes — but do
not commit and do not treat those errors as a checkpoint.

### Phase A — `shell.rs`: the engine

**Interfaces.** Consumes: `StreamMsg` (from `stream.rs`). Produces, in `crate::cmd::agent::cli::shell`:

```rust
pub const SHELL_MAX_BYTES: usize = 8 * 1024;
pub const SHELL_CARD_MAX_BYTES: usize = 256 * 1024;
pub const KILL_GRACE: Duration = Duration::from_secs(2);
pub const DRAIN_INCOMPLETE_NOTE: &str =
    "[output may be incomplete — a background process still holds this command's pipes]";
pub fn cap_tail(text: &str, max: usize) -> String;
pub fn signal_group(pid: u32, sig: i32);
pub const SIGTERM_NUM: i32;  pub const SIGKILL_NUM: i32;
pub enum ShellEnd { Exited(i32), Signaled(Option<i32>), Cancelled, SpawnFailed(String) }
impl ShellEnd { pub fn card_tail(&self) -> Option<String>; pub fn reaches_agent(&self) -> bool; }
pub fn spawn(cmd: &str) -> std::io::Result<(tokio::process::Child, u32)>;
pub async fn run(
    child: tokio::process::Child, pid: u32, gen: u64,
    tx: mpsc::Sender<StreamMsg>, cancel: oneshot::Receiver<()>,
) -> ShellEnd;
// stream.rs gains:
//   StreamMsg::ShellOutput { gen: u64, chunk: String }
//   StreamMsg::ShellDone   { gen: u64, cmd: String, end: ShellEnd }   (shape CHANGED)
```

- [x] In `stream.rs`, replace the `ShellDone` variant and add `ShellOutput`:

```rust
    /// A chunk of a running local `!command`'s output (stdout and stderr
    /// interleaved in arrival order, as a terminal shows them). `gen` is the
    /// shell generation it belongs to; the UI drops a retired one (D8).
    /// Turn-independent like `Note`.
    ShellOutput { gen: u64, chunk: String },
    /// A local `!command` ended. The output is not here — it was streamed,
    /// and the live card owns it. Guaranteed to arrive after every chunk the
    /// command produced (D9).
    ShellDone {
        gen: u64,
        cmd: String,
        end: super::shell::ShellEnd,
    },
```

- [x] In `StreamMsg::task_id`, add the new variant to the turn-independent arm:

```rust
            StreamMsg::Note(_)
            | StreamMsg::Expired { .. }
            | StreamMsg::ShellOutput { .. }
            | StreamMsg::ShellDone { .. } => None,
```

- [x] In `stream.rs`, delete `SHELL_MAX_BYTES`, `SHELL_TIMEOUT_SECS`, the whole `run_local_shell` function, and its two tests (`run_local_shell_captures_output_and_exit`, `run_local_shell_truncates_huge_output`). Their replacements live in `shell.rs` below. `StreamMsg` must derive nothing new: confirm `#[derive(Debug)]` still compiles once `ShellEnd` derives `Debug`.

- [x] Append to `shell.rs` (after the Task 0 items):

```rust
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, oneshot};

use super::stream::StreamMsg;

/// Cap on the `!command` block forwarded to the agent. Value unchanged from
/// the buffered implementation; what changed is which end survives (§1.4).
pub const SHELL_MAX_BYTES: usize = 8 * 1024;
/// Cap on what one `!command` card keeps in the transcript. Far larger than
/// the agent's block — a human scrolls, a model pays per token — but not
/// unbounded: `!yes` would otherwise grow the TUI without end (D6).
pub const SHELL_CARD_MAX_BYTES: usize = 256 * 1024;
/// SIGTERM → this → SIGKILL, on the whole group (D5).
pub const KILL_GRACE: Duration = Duration::from_secs(2);
/// After the child exits, how long to keep draining its pipes. A grandchild
/// can hold them open past the shell's own exit, so this must be bounded or
/// a `!cmd` that spawned a daemon would never finish.
const DRAIN_GRACE: Duration = Duration::from_millis(250);
/// What the card says when that bound is what ended the drain. Silence here
/// would be a truncation the user cannot see (D9).
pub const DRAIN_INCOMPLETE_NOTE: &str =
    "[output may be incomplete — a background process still holds this command's pipes]";
const READ_BUF: usize = 8 * 1024;

/// Marker charged against the budget, not added on top of it (D6).
const TRUNCATED: &str = "[output truncated]\n";

/// Keep the last bytes of `text` that fit in `max`, on a char boundary,
/// prefixed with a marker when anything was dropped. The result is **always**
/// `<= max`: the marker comes out of the budget.
///
/// The tail, not the head: the exit marker and a test run's verdict are the
/// last lines written, and the old head-keeping cap dropped exactly those —
/// a failing command over the cap reached the agent looking clean (§1.4).
pub fn cap_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    // Nudging `cut` forward to a char boundary only ever shrinks the result,
    // so both branches stay within the cap.
    if max <= TRUNCATED.len() {
        // No room to say anything about the truncation; keep what fits.
        let mut cut = text.len() - max;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        return text[cut..].to_string();
    }
    let room = max - TRUNCATED.len();
    let mut cut = text.len() - room;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    format!("{TRUNCATED}{}", &text[cut..])
}

/// How a `!command` ended (D10). One enum rather than `(Option<i32>, bool)`,
/// which could not tell a signal from a spawn failure from a missing code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEnd {
    /// Ran to completion with this status code.
    Exited(i32),
    /// A signal ended it. `Some` where the platform reports the number.
    Signaled(Option<i32>),
    /// The user stopped it — Ctrl-C, quit, `/clear`, or a channel switch.
    Cancelled,
    /// The shell could not be started at all.
    SpawnFailed(String),
}

impl ShellEnd {
    /// The line stamped on the card, if this ending deserves one. A clean
    /// exit does not: `$ ls` followed by output reads better than `[exit 0]`.
    pub fn card_tail(&self) -> Option<String> {
        match self {
            ShellEnd::Exited(0) => None,
            ShellEnd::Exited(code) => Some(format!("[exit {code}]")),
            ShellEnd::Signaled(Some(n)) => Some(format!("[killed by signal {n}]")),
            ShellEnd::Signaled(None) => Some("[killed by a signal]".to_string()),
            ShellEnd::Cancelled => Some("[cancelled]".to_string()),
            ShellEnd::SpawnFailed(e) => Some(format!("[failed to run: {e}]")),
        }
    }

    /// D4: everything reaches the agent except a run the user called off.
    /// A spawn failure does reach it — "command not found" is exactly the
    /// kind of thing the agent should hear about.
    pub fn reaches_agent(&self) -> bool {
        !matches!(self, ShellEnd::Cancelled)
    }
}

#[cfg(unix)]
pub const SIGTERM_NUM: i32 = libc::SIGTERM;
#[cfg(unix)]
pub const SIGKILL_NUM: i32 = libc::SIGKILL;
#[cfg(not(unix))]
pub const SIGTERM_NUM: i32 = 0;
#[cfg(not(unix))]
pub const SIGKILL_NUM: i32 = 0;

/// Signal a whole process group.
///
/// Deliberately a local copy rather than `mur-agent-runtime`'s: the shared
/// part is three libc calls, the surrounding shapes differ (a job table and
/// a watch channel there, one child and a oneshot here), and making that
/// crate's private helper public would couple this TUI to the agent's
/// tool-execution internals. The group exists at all for the reason recorded
/// as D9 in the 2026-09-12 bash-yield spec: killing the shell alone leaves
/// `cargo`'s `rustc` children running.
#[cfg(unix)]
pub fn signal_group(pid: u32, sig: i32) {
    // SAFETY: `pid` came from `Child::id()` of a child spawned with
    // `process_group(0)`, so it is also the group id. `killpg` on a raw pgid
    // has no memory-safety hazard; a group that is already gone gives ESRCH.
    unsafe {
        libc::killpg(pid as libc::pid_t, sig);
    }
}

/// Windows: a tree kill, which reaches children but — unlike a Job Object —
/// does not bind a process that deliberately detaches. Spec §3.7 states that
/// weaker guarantee rather than pretending D5 holds identically here.
#[cfg(not(unix))]
pub fn signal_group(pid: u32, _sig: i32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Start `cmd` under the user's shell, as its own process group.
///
/// Synchronous and separate from [`run`] so the caller holds the pid before
/// the child moves into a task: the quit path signals the group directly and
/// cannot wait for an async hop (§3.5).
pub fn spawn(cmd: &str) -> std::io::Result<(tokio::process::Child, u32)> {
    use tokio::process::Command;
    #[cfg(unix)]
    let (shell, flag) = (
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
        "-c",
    );
    #[cfg(windows)]
    let (shell, flag) = (
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd".into()),
        "/C",
    );
    let mut command = Command::new(shell);
    command
        .arg(flag)
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // D5: its own group, so a kill reaches `cargo`'s children and not just
    // the shell. A refusal fails the spawn — never an ungrouped child, which
    // would make every later kill lie about what it ended.
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn()?;
    let pid = child.id().unwrap_or(0);
    Ok((child, pid))
}

/// Read one pipe to EOF, forwarding decoded text.
///
/// The decode is per read, and a read boundary is chosen by the kernel, so a
/// multi-byte character can straddle two reads. `carry` holds an incomplete
/// trailing sequence back until the next read rather than emitting a
/// replacement character for a character that is perfectly fine. One carry
/// per pipe, never shared: interleaving two streams' bytes through a single
/// carry would corrupt both.
async fn pump<R: tokio::io::AsyncRead + Unpin>(mut r: R, tx: mpsc::Sender<String>) {
    let mut buf = vec![0u8; READ_BUF];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        carry.extend_from_slice(&buf[..n]);
        let good = match std::str::from_utf8(&carry) {
            Ok(_) => carry.len(),
            Err(e) => e.valid_up_to(),
        };
        if good > 0 {
            let s = String::from_utf8_lossy(&carry[..good]).into_owned();
            carry.drain(..good);
            if tx.send(s).await.is_err() {
                return;
            }
        }
        // Nothing valid and more than one max-length sequence held back: the
        // leading bytes are genuinely not UTF-8, not merely incomplete. Flush
        // lossily so `carry` cannot grow without bound on binary output.
        if good == 0 && carry.len() > 4 {
            let s = String::from_utf8_lossy(&carry).into_owned();
            carry.clear();
            if tx.send(s).await.is_err() {
                return;
            }
        }
    }
    if !carry.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&carry).into_owned()).await;
    }
}

#[cfg(unix)]
fn end_of(status: std::process::ExitStatus) -> ShellEnd {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(c) => ShellEnd::Exited(c),
        None => ShellEnd::Signaled(status.signal()),
    }
}

#[cfg(not(unix))]
fn end_of(status: std::process::ExitStatus) -> ShellEnd {
    match status.code() {
        Some(c) => ShellEnd::Exited(c),
        None => ShellEnd::Signaled(None),
    }
}

/// Drive a spawned `!command` to its end, streaming output as it arrives.
///
/// Not bounded by any clock (D1): it returns when the command finishes or
/// when `cancel` fires, and nothing else. Every chunk the command produced
/// has been forwarded by the time this returns, so the `ShellDone` the caller
/// sends afterwards can never overtake output on the same ordered channel
/// (D9).
pub async fn run(
    child: tokio::process::Child,
    pid: u32,
    gen: u64,
    tx: mpsc::Sender<StreamMsg>,
    cancel: oneshot::Receiver<()>,
) -> ShellEnd {
    let mut child = child;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // One channel, two readers. Both clones must be moved into the tasks so
    // that EOF on both pipes closes the channel — a stray clone here would
    // make the drain below wait out its whole grace period every time.
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    match (stdout, stderr) {
        (Some(o), Some(e)) => {
            tokio::spawn(pump(o, out_tx.clone()));
            tokio::spawn(pump(e, out_tx));
        }
        (Some(o), None) => {
            tokio::spawn(pump(o, out_tx));
        }
        (None, Some(e)) => {
            tokio::spawn(pump(e, out_tx));
        }
        (None, None) => drop(out_tx),
    }

    let (done_tx, mut done_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = done_tx.send(child.wait().await);
    });

    let send = |tx: &mpsc::Sender<StreamMsg>, chunk: String| {
        let tx = tx.clone();
        async move { tx.send(StreamMsg::ShellOutput { gen, chunk }).await }
    };

    let mut cancel = cancel;
    let mut cancelled = false;
    let mut end: Option<ShellEnd> = None;
    // `Some` only between SIGTERM and its SIGKILL; dropped when the loop ends.
    let mut escalate: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    loop {
        tokio::select! {
            Some(chunk) = out_rx.recv() => {
                if send(&tx, chunk).await.is_err() {
                    break; // UI gone
                }
            }
            r = &mut done_rx => {
                end = Some(match r {
                    Ok(Ok(status)) => end_of(status),
                    _ => ShellEnd::Signaled(None),
                });
                break;
            }
            _ = &mut cancel, if !cancelled => {
                cancelled = true;
                signal_group(pid, SIGTERM_NUM);
                // D5: the escalation is OWNED BY THIS LOOP, not detached. A
                // child that exits inside the grace breaks below and drops
                // this timer, so no delayed `killpg` is ever left in flight —
                // which also means we never signal a pgid we have stopped
                // owning, as a recycled pid would make somebody else's.
                escalate = Some(Box::pin(tokio::time::sleep(KILL_GRACE)));
            }
            () = async { escalate.as_mut().expect("guarded by the condition").await },
                 if escalate.is_some() =>
            {
                signal_group(pid, SIGKILL_NUM);
                escalate = None; // fires once
            }
        }
    }

    // D9: the child is gone but a grandchild may still hold the pipes, so the
    // drain is bounded — and when the bound is what stopped it, the card says
    // so instead of losing the tail in silence.
    let drained = tokio::time::timeout(DRAIN_GRACE, async {
        while let Some(chunk) = out_rx.recv().await {
            if send(&tx, chunk).await.is_err() {
                return;
            }
        }
    })
    .await;
    if drained.is_err() {
        let _ = send(&tx, DRAIN_INCOMPLETE_NOTE.to_string()).await;
    }

    if cancelled {
        ShellEnd::Cancelled
    } else {
        end.unwrap_or(ShellEnd::Signaled(None))
    }
}
```

- [x] Add to `shell.rs`'s test module:

```rust
    use super::*;
    use crate::cmd::agent::cli::stream::StreamMsg;

    /// Drive a command to completion, collecting every streamed chunk.
    #[cfg(unix)]
    async fn drive(cmd: &str) -> (String, ShellEnd) {
        let (tx, mut rx) = mpsc::channel(256);
        let (_c_tx, c_rx) = oneshot::channel();
        let (child, pid) = spawn(cmd).expect("spawn");
        let end = run(child, pid, 1, tx, c_rx).await;
        let mut text = String::new();
        while let Ok(m) = rx.try_recv() {
            if let StreamMsg::ShellOutput { chunk, .. } = m {
                text.push_str(&chunk);
            }
        }
        (text, end)
    }

    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 probes existence only.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn streams_output_and_reports_exit() {
        let (text, end) = drive("echo hi; echo err >&2; exit 3").await;
        assert!(text.contains("hi"), "{text}");
        assert!(text.contains("err"), "{text}");
        assert_eq!(end, ShellEnd::Exited(3));
    }

    /// Test 1 — D1: a command that outlives the deleted 30 s ceiling still
    /// finishes on its own terms.
    #[cfg(unix)]
    #[tokio::test]
    async fn no_clock_ends_a_slow_command() {
        let t0 = std::time::Instant::now();
        let (text, end) = drive("sleep 1.5; echo late").await;
        assert!(t0.elapsed() >= Duration::from_millis(1400));
        assert!(text.contains("late"), "{text}");
        assert_eq!(end, ShellEnd::Exited(0));
    }

    /// Test 2 — D2: output arrives BEFORE the command ends. The assertion
    /// that separates streaming from buffering.
    #[cfg(unix)]
    #[tokio::test]
    async fn output_arrives_before_the_command_ends() {
        let (tx, mut rx) = mpsc::channel(256);
        let (_c_tx, c_rx) = oneshot::channel();
        let (child, pid) = spawn("echo one; sleep 0.6; echo two").expect("spawn");
        let task = tokio::spawn(run(child, pid, 1, tx, c_rx));
        let first = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("a chunk before the command finished")
            .expect("channel open");
        assert!(!task.is_finished(), "the command already ended: not streaming");
        match first {
            StreamMsg::ShellOutput { chunk, .. } => assert!(chunk.contains("one"), "{chunk}"),
            other => panic!("expected ShellOutput, got {other:?}"),
        }
        assert_eq!(task.await.unwrap(), ShellEnd::Exited(0));
    }

    /// Test 14 — D9: the last chunk always precedes the end. Here the
    /// command writes immediately before exiting, the case where a naive
    /// implementation finalises on child-exit and loses it.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_last_chunk_is_forwarded_before_run_returns() {
        let (tx, mut rx) = mpsc::channel(256);
        let (_c_tx, c_rx) = oneshot::channel();
        let (child, pid) = spawn("echo FINAL").expect("spawn");
        let end = run(child, pid, 1, tx, c_rx).await;
        assert_eq!(end, ShellEnd::Exited(0));
        // `run` has returned: everything it will ever send is already queued,
        // so a `ShellDone` sent now cannot overtake it on this channel.
        let mut text = String::new();
        while let Ok(StreamMsg::ShellOutput { chunk, .. }) = rx.try_recv() {
            text.push_str(&chunk);
        }
        assert!(text.contains("FINAL"), "{text}");
    }

    /// Tests 3 + 4 — D5: cancel kills the whole group (the grandchild dies,
    /// not just the shell) and returns promptly, not after the 60 s sleep.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_the_group_promptly() {
        let (tx, mut rx) = mpsc::channel(256);
        let (c_tx, c_rx) = oneshot::channel();
        let (child, pid) = spawn("sleep 60 & echo $!; wait").expect("spawn");
        let task = tokio::spawn(run(child, pid, 1, tx, c_rx));
        let chunk = loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("grandchild pid line")
                .expect("channel open")
            {
                StreamMsg::ShellOutput { chunk, .. } if !chunk.trim().is_empty() => break chunk,
                _ => continue,
            }
        };
        let grandchild: u32 = chunk.trim().parse().expect("a pid on stdout");
        assert!(alive(grandchild), "grandchild should be running");

        let t0 = std::time::Instant::now();
        c_tx.send(()).unwrap();
        let end = tokio::time::timeout(KILL_GRACE * 3, task)
            .await
            .expect("cancel returned promptly, not after the 60s sleep")
            .unwrap();
        assert_eq!(end, ShellEnd::Cancelled);
        assert!(t0.elapsed() < KILL_GRACE * 3);

        let deadline = std::time::Instant::now() + KILL_GRACE + Duration::from_secs(1);
        while std::time::Instant::now() < deadline && alive(grandchild) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!alive(grandchild), "grandchild {grandchild} outlived the cancel");
    }

    /// Test 9 — a multi-byte character split across two reads is decoded
    /// once, not as two replacement characters.
    #[cfg(unix)]
    #[tokio::test]
    async fn utf8_split_across_reads_survives() {
        let (text, _) = drive("printf '\\xe2'; sleep 0.3; printf '\\x9c\\x93'").await;
        assert_eq!(text, "✓", "got {text:?}");
    }

    /// Test 10 — D10: each ending writes its own card tail, and only a
    /// cancelled run is withheld from the agent.
    #[test]
    fn every_end_has_its_own_tail_and_routing() {
        assert_eq!(ShellEnd::Exited(0).card_tail(), None);
        assert_eq!(ShellEnd::Exited(2).card_tail().unwrap(), "[exit 2]");
        assert_eq!(
            ShellEnd::Signaled(Some(9)).card_tail().unwrap(),
            "[killed by signal 9]"
        );
        assert_eq!(
            ShellEnd::Signaled(None).card_tail().unwrap(),
            "[killed by a signal]"
        );
        assert_eq!(ShellEnd::Cancelled.card_tail().unwrap(), "[cancelled]");
        assert_eq!(
            ShellEnd::SpawnFailed("nope".into()).card_tail().unwrap(),
            "[failed to run: nope]"
        );
        for e in [
            ShellEnd::Exited(0),
            ShellEnd::Exited(1),
            ShellEnd::Signaled(Some(9)),
            ShellEnd::SpawnFailed("nope".into()),
        ] {
            assert!(e.reaches_agent(), "{e:?}");
        }
        assert!(!ShellEnd::Cancelled.reaches_agent());
    }

    /// Test 8 — D6 + §1.4: the cap keeps the TAIL, leads with the marker, and
    /// the marker is INSIDE the budget. The old version returned `max` bytes
    /// plus 19, so "the block is <= SHELL_MAX_BYTES" was false by a marker.
    #[test]
    fn cap_tail_keeps_the_end_within_the_budget() {
        let text = format!("{}\n[exit 1]", "x".repeat(100));
        let out = cap_tail(&text, 40);
        assert!(out.starts_with(TRUNCATED), "{out}");
        assert!(out.ends_with("[exit 1]"), "{out}");
        assert!(out.len() <= 40, "busted the cap: {} > 40", out.len());
        assert_eq!(cap_tail("short", 40), "short");
    }

    /// Boundary: a cap with no room for the marker still honours the cap,
    /// by dropping the marker rather than the promise.
    #[test]
    fn cap_tail_below_the_marker_length_still_fits() {
        let text = "y".repeat(100);
        for max in [1usize, 5, TRUNCATED.len(), TRUNCATED.len() + 1] {
            let out = cap_tail(&text, max);
            assert!(out.len() <= max, "max={max}: {} bytes", out.len());
        }
    }

    /// A multi-byte tail is never split: the result stays valid UTF-8 and
    /// still fits.
    #[test]
    fn cap_tail_never_splits_a_character() {
        let text = "✓".repeat(50); // 3 bytes each
        for max in 20..40 {
            let out = cap_tail(&text, max);
            assert!(out.len() <= max, "max={max}");
            assert!(out.strip_prefix(TRUNCATED).unwrap_or(&out).chars().all(|c| c == '✓'));
        }
    }
```

- [x] No commit, no clippy gate yet — `mod.rs` and `app.rs` still use the old `ShellDone` shape and `run_local_shell`, so the crate does not build until phase C. Proceed to phase B.

---

### Phase B — `ShellState` and the live card

**Interfaces.** Consumes: `ShellEnd`, `signal_group`, `SIGTERM_NUM`, `SIGKILL_NUM`, `cap_tail`, `SHELL_CARD_MAX_BYTES` (T1). Produces:

```rust
// shell.rs
pub struct ShellState { /* private */ }
impl ShellState {
    pub fn is_running(&self) -> bool;
    pub fn generation(&self) -> u64;
    pub fn accepts(&self, gen: u64) -> bool;
    pub fn begin(&mut self, pid: u32, cancel: oneshot::Sender<()>) -> Option<u64>;
    pub fn finish(&mut self, gen: u64);
}
pub fn cancel(state: &mut ShellState, hard: bool) -> bool;   // true = something was stopped
impl ShellState { pub fn done(&mut self, gen: u64); }        // replaces `finish`
// app.rs
pub shell: shell::ShellState,             // App field
impl App {
    pub fn begin_shell(&mut self, cmd: &str);
    pub fn append_shell_output(&mut self, chunk: &str);
    pub fn finish_shell(&mut self, end: &shell::ShellEnd) -> String;
}
```

`push_shell` is **removed** — `begin_shell` + `finish_shell` replace it.

- [x] Append to `shell.rs`:

```rust
/// The one `!command` slot (D7: single-flight), its generation (D8), and —
/// crucially — the pid of a command that has been signalled but not yet
/// confirmed dead.
///
/// Two questions, two answers. "May this still touch the UI?" is the
/// generation. "Is this process still ours to kill?" is the slot. Collapsing
/// them is what let a quit two seconds after a Ctrl-C leave an orphaned
/// process group (§7, round 2, finding 2).
#[derive(Default)]
pub struct ShellState {
    gen: u64,
    slot: Slot,
}

#[derive(Default)]
enum Slot {
    #[default]
    Idle,
    /// Live: Ctrl-C ends it and the spinner ticks for it.
    Running {
        gen: u64,
        pid: u32,
        cancel: oneshot::Sender<()>,
    },
    /// Signalled, not yet reaped. The UI has moved on — the card is already
    /// finalised (D11) and this generation retired — but the pid stays so a
    /// quit inside the grace window still has a group to kill.
    Cancelling { gen: u64, pid: u32 },
}

impl Slot {
    fn gen(&self) -> Option<u64> {
        match self {
            Slot::Idle => None,
            Slot::Running { gen, .. } | Slot::Cancelling { gen, .. } => Some(*gen),
        }
    }

    fn pid(&self) -> Option<u32> {
        match self {
            Slot::Idle => None,
            Slot::Running { pid, .. } | Slot::Cancelling { pid, .. } => Some(*pid),
        }
    }
}

impl ShellState {
    /// A command the user can still Ctrl-C, and that the spinner ticks for.
    /// A `Cancelling` one is neither: its card is already finalised.
    pub fn is_running(&self) -> bool {
        matches!(self.slot, Slot::Running { .. })
    }

    pub fn generation(&self) -> u64 {
        self.gen
    }

    /// May an event from `gen` still touch the UI? A teardown retires the
    /// generation, so anything the dying task emits afterwards is dropped
    /// rather than written into a cleared transcript or another channel (D8).
    pub fn accepts(&self, gen: u64) -> bool {
        gen == self.gen
    }

    /// Claim the slot. `None` when one is already held — the caller refuses
    /// *before spawning* (D7).
    ///
    /// Never assign over a live handle: dropping an `oneshot::Sender`
    /// resolves its receiver, so an overwrite would silently cancel the
    /// command already running (§7, round 1, finding 1).
    pub fn begin(&mut self, pid: u32, cancel: oneshot::Sender<()>) -> Option<u64> {
        if !matches!(self.slot, Slot::Idle) {
            return None;
        }
        self.gen += 1;
        self.slot = Slot::Running {
            gen: self.gen,
            pid,
            cancel,
        };
        Some(self.gen)
    }

    /// The task reported the child is gone — whichever way it went. Called
    /// unconditionally on `ShellDone`, *including* for a retired generation,
    /// because this is the resource question, not the UI one: it is what
    /// clears `Cancelling` so quit stops trying to kill a dead group.
    pub fn done(&mut self, gen: u64) {
        if self.slot.gen() == Some(gen) {
            self.slot = Slot::Idle;
        }
    }
}

/// End whatever the slot holds and retire its generation (D8).
///
/// `hard` is the quit path: the event loop is about to stop, so nothing is
/// left to run a grace timer. Signal the group directly and synchronously,
/// and do it for a `Cancelling` slot too — a user who pressed Ctrl-C and then
/// quit within two seconds is exactly the case where the soft path's
/// escalation never gets to run (§3.5).
///
/// Returns whether anything was stopped, so the call site knows whether to
/// finalise a card (D11).
pub fn cancel(state: &mut ShellState, hard: bool) -> bool {
    let slot = std::mem::replace(&mut state.slot, Slot::Idle);
    let (gen, pid) = match (slot.gen(), slot.pid()) {
        (Some(g), Some(p)) => (g, p),
        _ => return false,
    };
    state.gen += 1;
    match slot {
        Slot::Running { cancel, .. } if !hard => {
            // Err = the command already exited and the receiver is gone.
            let _ = cancel.send(());
            // The pid stays ours until the task reports back.
            state.slot = Slot::Cancelling { gen, pid };
        }
        _ => {
            // Hard, or already cancelling: no grace, no waiting, no timer to
            // outlive us. TERM gives a fast-handling child its chance; KILL
            // guarantees the rest. The slot is left Idle — nothing survives
            // this that we would need to kill again.
            signal_group(pid, SIGTERM_NUM);
            signal_group(pid, SIGKILL_NUM);
        }
    }
    true
}
```

- [x] Add to `shell.rs`'s tests:

```rust
    /// Test 11 — D7: the second claim is refused and the first handle is
    /// left intact. The regression this exists for: assigning over the
    /// field drops the first sender, which resolves its receiver and would
    /// silently cancel the command already running.
    #[test]
    fn a_second_command_is_refused_and_the_first_survives() {
        let mut s = ShellState::default();
        let (tx1, mut rx1) = oneshot::channel();
        let gen1 = s.begin(111, tx1).expect("first claims the slot");
        let (tx2, _rx2) = oneshot::channel();
        assert!(s.begin(222, tx2).is_none(), "second refused");
        assert_eq!(s.generation(), gen1, "the refusal did not retire gen1");
        assert!(
            rx1.try_recv().is_err() && !matches!(rx1.try_recv(), Ok(())),
            "the first command was not cancelled"
        );
        assert!(s.is_running());
    }

    /// Test 12 — D8: a teardown retires the generation, so late events from
    /// the dying task are no longer accepted.
    #[test]
    fn cancel_retires_the_generation() {
        let mut s = ShellState::default();
        let (tx, mut rx) = oneshot::channel();
        let gen = s.begin(0, tx).unwrap();
        assert!(s.accepts(gen));
        assert!(cancel(&mut s, false), "something was stopped");
        assert!(!s.accepts(gen), "stale events are rejected");
        assert!(!s.is_running());
        assert_eq!(rx.try_recv(), Ok(()), "the soft path signalled the task");
        // Idempotent: a second press finds a Cancelling slot, kills it hard,
        // and a third finds nothing at all.
        assert!(cancel(&mut s, false));
        assert!(!cancel(&mut s, false), "nothing left to stop");
    }

    /// Tests 13 + 18 — §3.5, with a REAL process group, not a placeholder
    /// pid. Two regressions in one: a hard quit must kill the group
    /// synchronously (the detached timer dies with the runtime), and it must
    /// still find the group when the user pressed Ctrl-C moments earlier —
    /// the soft path deliberately keeps the pid in `Cancelling` for exactly
    /// this.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_quit_after_a_cancel_still_kills_the_group() {
        // A group that ignores SIGTERM, so only the SIGKILL can end it —
        // which is the whole point: a polite signal would pass either way.
        let (child, pid) = spawn("trap '' TERM; sleep 60 & echo $!; wait").expect("spawn");
        let (tx, mut rx) = mpsc::channel(64);
        let (c_tx, c_rx) = oneshot::channel();
        let task = tokio::spawn(run(child, pid, 1, tx, c_rx));
        let chunk = loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("pid line")
                .expect("open")
            {
                StreamMsg::ShellOutput { chunk, .. } if !chunk.trim().is_empty() => break chunk,
                _ => continue,
            }
        };
        let grandchild: u32 = chunk.trim().parse().expect("a pid");

        let mut state = ShellState::default();
        state.begin(pid, c_tx).expect("slot");

        // Ctrl-C: soft. The pid must survive into `Cancelling`.
        assert!(cancel(&mut state, false), "something was stopped");
        assert!(!state.is_running());

        // Quit, well inside the grace window — the case that orphaned.
        assert!(cancel(&mut state, true), "the cancelling slot was still killable");

        for p in [pid, grandchild] {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < deadline && alive(p) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(!alive(p), "pid {p} survived a hard quit");
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
    }

    /// `done` clears a `Cancelling` slot even though its generation is
    /// retired — the resource question is not the UI question (D8).
    #[test]
    fn done_clears_a_cancelling_slot_despite_the_retired_generation() {
        let mut s = ShellState::default();
        let (tx, _rx) = oneshot::channel();
        let gen = s.begin(0, tx).unwrap();
        cancel(&mut s, false);
        assert!(!s.accepts(gen), "the UI has moved on");
        s.done(gen);
        assert!(!s.is_running());
        // Nothing left to kill: a later hard quit is a no-op.
        assert!(!cancel(&mut s, true), "the slot was already empty");
    }

    /// A natural end frees the slot but keeps the generation, because its own
    /// `ShellDone` still has to be accepted.
    #[test]
    fn finish_frees_the_slot_without_retiring_the_generation() {
        let mut s = ShellState::default();
        let (tx, _rx) = oneshot::channel();
        let gen = s.begin(0, tx).unwrap();
        s.done(gen);
        assert!(!s.is_running());
        assert!(s.accepts(gen), "its own ShellDone is still wanted");
    }
```

- [x] In `app.rs`, add the field immediately after `pub ctrl_c_hint: bool,`:

```rust
    /// The running `!cmd`, if any, and its generation. `is_running()` is what
    /// makes Ctrl-C end the shell rather than the agent turn (D3), and what
    /// keeps the spinner ticking for a shell-only command (§3.6).
    pub shell: super::shell::ShellState,
```

- [x] Add `shell: Default::default(),` to the single `App` construction site, immediately after `ctrl_c_hint: false,` (grep `ctrl_c_hint:` — exactly two hits, the field and this one).

- [x] Replace the whole `push_shell` method with these three:

```rust
    /// Open the live card for a `!cmd` that was just accepted. The card
    /// exists from the keypress (D2), so a silent command is still visibly
    /// running, and `append_shell_output` always has a target.
    pub fn begin_shell(&mut self, cmd: &str) {
        let mut m = ChatMsg::new(Role::Shell, format!("$ {cmd}"));
        m.streaming = true;
        self.messages.push(m);
        self.scroll_back = 0;
    }

    /// The live shell card, if one is open.
    fn streaming_shell_mut(&mut self) -> Option<&mut ChatMsg> {
        self.messages
            .iter_mut()
            .rev()
            .find(|m| m.role == Role::Shell && m.streaming)
    }

    /// Append streamed `!cmd` output to the live card (D2), head-dropping
    /// past `SHELL_CARD_MAX_BYTES` so a chatty command cannot grow the
    /// transcript without bound (D6).
    ///
    /// NB: does not reset `scroll_back` — same reason as `append_delta`. A
    /// user scrolled up to read earlier output must not be yanked back to
    /// the bottom by every new line.
    pub fn append_shell_output(&mut self, chunk: &str) {
        let Some(m) = self.streaming_shell_mut() else {
            return; // no live card (a teardown cleared it); drop the chunk
        };
        if !m.text.is_empty() && !m.text.ends_with('\n') {
            m.text.push('\n');
        }
        m.text.push_str(chunk);
        if m.text.len() > super::shell::SHELL_CARD_MAX_BYTES {
            // The `$ cmd` line is the card's identity; keep it above the
            // truncation marker rather than letting the tail eat it.
            let first = m.text.lines().next().unwrap_or_default().to_string();
            let rest = m.text.split_once('\n').map(|(_, r)| r).unwrap_or_default();
            let kept = super::shell::cap_tail(rest, super::shell::SHELL_CARD_MAX_BYTES);
            m.text = format!("{first}\n{kept}");
        }
    }

    /// Close the live `!cmd` card: stamp how it ended, stop the spinner,
    /// persist it, and hand back the output body for the agent block.
    pub fn finish_shell(&mut self, end: &super::shell::ShellEnd) -> String {
        let Some(m) = self.streaming_shell_mut() else {
            return String::new();
        };
        if let Some(tail) = end.card_tail() {
            if !m.text.ends_with('\n') {
                m.text.push('\n');
            }
            m.text.push_str(&tail);
        }
        m.streaming = false;
        let text = m.text.trim_end().to_string();
        m.text = text.clone();
        self.persist_turn("shell", &text, None, &[]);
        // The block wants the output alone; the card's first line is `$ cmd`
        // and `shell_block` re-adds it.
        text.split_once('\n')
            .map(|(_, r)| r.to_string())
            .unwrap_or_default()
    }
```

- [x] Add to `app.rs`'s `step_app_tests` module (it already has `use super::*;` and a local `fn app() -> App`). If `super::shell::` does not resolve from inside that module, use `crate::cmd::agent::cli::shell::` — both name the same module; pick whichever compiles and use it consistently:

```rust
    /// Test 10 (card half) — the card opens on the keypress, accumulates,
    /// and stamps a non-zero exit; a clean exit stamps nothing.
    #[test]
    fn shell_card_opens_accumulates_and_stamps_exit() {
        use crate::cmd::agent::cli::shell::ShellEnd;
        let mut a = app();
        a.begin_shell("cargo test");
        let card = a.messages.last().expect("card");
        assert_eq!(card.text, "$ cargo test");
        assert!(card.streaming, "the card is live");

        a.append_shell_output("running 3 tests");
        a.append_shell_output("test result: FAILED");
        let body = a.finish_shell(&ShellEnd::Exited(1));
        let card = a.messages.last().expect("card");
        assert!(!card.streaming, "the card is finalised");
        assert_eq!(
            card.text,
            "$ cargo test\nrunning 3 tests\ntest result: FAILED\n[exit 1]"
        );
        assert_eq!(body, "running 3 tests\ntest result: FAILED\n[exit 1]");

        let mut b = app();
        b.begin_shell("true");
        b.append_shell_output("ok");
        b.finish_shell(&ShellEnd::Exited(0));
        assert_eq!(b.messages.last().unwrap().text, "$ true\nok");
    }

    /// Test 15 — D2: a silent command still has a live card, immediately.
    #[test]
    fn a_silent_command_still_shows_a_live_card() {
        let mut a = app();
        a.begin_shell("sleep 45");
        let card = a.messages.last().expect("card");
        assert_eq!(card.text, "$ sleep 45");
        assert!(card.streaming, "live before any output exists");
    }

    /// D4 render half: a cancelled card says so.
    #[test]
    fn cancelled_shell_card_is_marked() {
        use crate::cmd::agent::cli::shell::ShellEnd;
        let mut a = app();
        a.begin_shell("sleep 60");
        a.append_shell_output("partial");
        let body = a.finish_shell(&ShellEnd::Cancelled);
        assert_eq!(
            a.messages.last().unwrap().text,
            "$ sleep 60\npartial\n[cancelled]"
        );
        assert!(body.contains("[cancelled]"));
    }

    /// Test 7 — D6: the card cap keeps the tail and never eats `$ cmd`.
    #[test]
    fn shell_card_cap_keeps_the_tail_and_the_command_line() {
        use crate::cmd::agent::cli::shell::SHELL_CARD_MAX_BYTES;
        let mut a = app();
        a.begin_shell("noisy");
        a.append_shell_output(&"x".repeat(SHELL_CARD_MAX_BYTES + 1024));
        a.append_shell_output("LAST");
        let text = &a.messages.last().unwrap().text;
        assert!(text.starts_with("$ noisy\n"), "command line survived");
        assert!(text.ends_with("LAST"), "tail survived");
        assert!(text.contains("[output truncated]"));
        assert!(text.len() < SHELL_CARD_MAX_BYTES + 512);
    }

    /// An empty-output command still leaves exactly one card, and a teardown
    /// that cleared the transcript leaves nothing for a late chunk to hit.
    #[test]
    fn empty_output_leaves_one_card_and_a_cleared_one_absorbs_late_chunks() {
        use crate::cmd::agent::cli::shell::ShellEnd;
        let mut a = app();
        a.begin_shell("true");
        a.finish_shell(&ShellEnd::Exited(0));
        assert_eq!(
            a.messages.iter().filter(|m| m.role == Role::Shell).count(),
            1
        );
        assert_eq!(a.messages.last().unwrap().text, "$ true");

        a.messages.clear(); // what /clear does
        a.append_shell_output("late");
        assert!(a.messages.is_empty(), "a late chunk found no card and was dropped");
    }
```

- [x] Still no commit — `mod.rs` closes the cutover in phase C, and only then does the crate build. Proceed.

---

### Phase C — `mod.rs`: wiring, the four teardown sites, the spinner

**Interfaces.** Consumes everything from T1 and T2. Produces: `route_shell_output(cancelled, streaming, task_id, over_budget)`.

- [x] In `shell.rs`, give the moved `route_shell_output` its new leading parameter and first arm:

```rust
/// Pure so the four routes are testable without pricing or a live agent.
/// The budget gates a NEW turn only, exactly as `submit` does for typed text:
/// a steer rides the turn already being paid for.
pub(super) fn route_shell_output(
    cancelled: bool,
    streaming: bool,
    task_id: Option<&str>,
    over_budget: bool,
) -> ShellRoute {
    // D4: Ctrl-C means "never mind". It outranks every other route — waking
    // the model with half a test run is the opposite of what the key said.
    if cancelled {
        return ShellRoute::Skip("cancelled — not sent to the agent");
    }
    if streaming {
```

  …the rest of the body is unchanged. Update the moved tests' calls to pass a leading `false`.

- [x] In `submit`, replace the `!command` arm's body:

```rust
    if let Some(cmd) = trimmed.strip_prefix('!').map(str::trim)
        && !cmd.is_empty()
    {
        app.clear_input();
        // D7: one foreground job. Refuse before spawning — and never assign
        // over a live handle, which would drop its sender and silently
        // cancel the command already running.
        if app.shell.is_running() {
            app.push_system("a `!command` is already running — Ctrl-C to stop it");
            return;
        }
        let (child, pid) = match shell::spawn(cmd) {
            Ok(v) => v,
            Err(e) => {
                // Nothing to cancel and nothing to stream: one finished card.
                app.begin_shell(cmd);
                let end = shell::ShellEnd::SpawnFailed(e.to_string());
                let output = app.finish_shell(&end);
                finish_shell_turn(app, cmd, &end, output, tx);
                return;
            }
        };
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let Some(gen) = app.shell.begin(pid, cancel_tx) else {
            // Unreachable given the guard above; refuse rather than leak.
            shell::signal_group(pid, shell::SIGKILL_NUM);
            return;
        };
        // D2: the card exists from the keypress, so `!sleep 45` is visibly
        // running rather than silent for 45 seconds.
        app.begin_shell(cmd);
        let (cmd, t) = (cmd.to_string(), tx.clone());
        tokio::spawn(async move {
            let end = shell::run(child, pid, gen, t.clone(), cancel_rx).await;
            let _ = t.send(StreamMsg::ShellDone { gen, cmd, end }).await;
        });
        return;
    }
```

- [x] Add `stop_shell` beside `start_shell_turn`. This is the D11 fix: the
  card is finalised **here**, synchronously, not by the `ShellDone` that
  arrives up to two seconds later carrying a generation this very call has
  just retired — which is why that card would otherwise have stayed
  `streaming` forever, unstamped, unpersisted, under a frozen footer:

```rust
/// End the running `!cmd` from any exit — Ctrl-C, quit, `/clear`, a channel
/// switch — and close its card on the spot (D8, D11).
///
/// `hard` is quit: signal the group synchronously, because nothing will be
/// left to run a grace timer. Nothing is routed to the agent either way: the
/// user stopped this on purpose (D4).
fn stop_shell(app: &mut App, hard: bool) {
    if !shell::cancel(&mut app.shell, hard) {
        return; // nothing was running
    }
    // Stamp `[cancelled]`, clear `streaming` (which also stops the footer
    // rendering as live), and persist. The late `ShellDone` will be dropped
    // by the generation check, so this is the only chance to do it.
    let _ = app.finish_shell(&shell::ShellEnd::Cancelled);
}
```

- [x] Add the shared finaliser beside `start_shell_turn` (used by both the spawn-failure path above and the `ShellDone` arm below, so the routing rule exists once):

```rust
/// Route a finished `!cmd`'s output: start a turn, steer the live one, or
/// say why it went nowhere. The card is already finalised by the caller.
fn finish_shell_turn(
    app: &mut App,
    cmd: &str,
    end: &shell::ShellEnd,
    output: String,
    tx: &mpsc::Sender<StreamMsg>,
) {
    let block = shell::shell_block(cmd, &shell::cap_tail(&output, shell::SHELL_MAX_BYTES));
    match shell::route_shell_output(
        !end.reaches_agent(),
        app.streaming,
        app.current_task_id.as_deref(),
        app.over_budget(),
    ) {
        ShellRoute::Start => start_shell_turn(app, block, tx),
        ShellRoute::Steer(task_id) => {
            let label = format!("$ {cmd} output");
            steer_now(app, task_id, block, &label, tx);
        }
        ShellRoute::Skip(why) => app.push_system(why),
    }
}
```

- [x] Replace the `ShellDone` arm of `handle_stream` and add a `ShellOutput` arm beside it:

```rust
        StreamMsg::ShellOutput { gen, chunk } => {
            // D8: a retired generation is a command the user already walked
            // away from; its output must not land in whatever conversation
            // is open now.
            if app.shell.accepts(gen) {
                app.append_shell_output(&chunk);
            }
        }
        StreamMsg::ShellDone { gen, cmd, end } => {
            // Unconditional: the child is gone, so its pid stops being ours
            // to kill. This is the resource question, and it must be answered
            // even for a generation the UI has retired — otherwise a quit
            // would keep signalling a dead group (§3.5).
            app.shell.done(gen);
            // Conditional: a retired generation has already had its card
            // finalised by `stop_shell` (D11), so there is nothing to draw
            // and nothing to route.
            if !app.shell.accepts(gen) {
                return;
            }
            let output = app.finish_shell(&end);
            finish_shell_turn(app, &cmd, &end, output, tx);
        }
```

- [x] In `handle_ctrl_c`, add a new **first** branch, above `if app.streaming`:

```rust
fn handle_ctrl_c(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    // D3: a running `!cmd` is what Ctrl-C ends — it is the thing the user
    // just launched and is watching. Any agent turn keeps running and still
    // has Esc-Esc. The state is retired here, so a second press falls
    // through to the behaviour below, unchanged.
    if app.shell.is_running() {
        stop_shell(app, false);
        return;
    }
    if app.streaming {
        // … unchanged …
```

- [x] Add the remaining three teardown sites (D8). In `request_quit`, before `app.should_quit = true;`:

```rust
    // Hard: the event loop is about to stop, so nothing is left to run the
    // escalation timer, and dropping the task would only `kill_on_drop` the
    // direct shell and leave its group. Covers a slot still `Cancelling`
    // from a Ctrl-C moments ago, which is the case that orphaned (§3.5).
    stop_shell(app, true);
```

  In the `SlashCmd::Clear` arm, immediately after the existing
  `cancel_in_flight(app, tx);` — the comment already there ("so its worker
  can't write into the fresh conversation after the reset") is the same
  reason, so put it under the same one:

```rust
            stop_shell(app, false);
```

  At the `app.switch_channel(&id)` call site (`mod.rs:2105`), immediately
  before the call:

```rust
                        stop_shell(app, false);
```

- [x] Change the event loop's spinner guard (`mod.rs:1039`) so a shell-only command animates (§3.6) — without this the footer renders once and freezes, which reads as hung:

```rust
            _ = spinner.tick(), if app.streaming || app.shell.is_running() => app.tick_spinner(),
```

- [x] The two existing tests `shell_done_while_idle_starts_a_turn_without_a_user_bubble` and `shell_done_while_streaming_steers_the_live_turn` construct `StreamMsg::ShellDone { cmd, output }`, which no longer exists. Rewrite their setup — the assertions that follow are unchanged:

```rust
    /// Idle: the block becomes the outgoing user message, the transcript keeps
    /// the one Shell card and gains no User bubble.
    #[tokio::test]
    async fn shell_done_while_idle_starts_a_turn_without_a_user_bubble() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        let (c_tx, _c_rx) = tokio::sync::oneshot::channel();
        let gen = app.shell.begin(0, c_tx).expect("slot");
        app.begin_shell("ls");
        app.append_shell_output("a\nb");
        handle_stream(
            &mut app,
            StreamMsg::ShellDone {
                gen,
                cmd: "ls".into(),
                end: shell::ShellEnd::Exited(0),
            },
            &tx,
        );
        assert_eq!(
            app.messages
                .iter()
                .filter(|m| m.role == Role::Shell)
                .count(),
            1
        );
        assert_eq!(
            app.messages.iter().filter(|m| m.role == Role::User).count(),
            0
        );
        assert!(app.streaming, "a turn started");
        let params = app.inflight_params.clone().expect("params kept for replay");
        let text = params["message"]["parts"][0]["text"].as_str().unwrap();
        assert!(text.contains("$ ls\na\nb"), "{text}");
        assert!(
            text.starts_with("[shell command the user ran locally]"),
            "{text}"
        );
    }

    /// Streaming: the block steers the live turn; no second turn starts.
    #[tokio::test]
    async fn shell_done_while_streaming_steers_the_live_turn() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        let before = app.begin_user_turn("working");
        let (c_tx, _c_rx) = tokio::sync::oneshot::channel();
        let gen = app.shell.begin(0, c_tx).expect("slot");
        app.begin_shell("ls");
        app.append_shell_output("a");
        handle_stream(
            &mut app,
            StreamMsg::ShellDone {
                gen,
                cmd: "ls".into(),
                end: shell::ShellEnd::Exited(0),
            },
            &tx,
        );
        assert_eq!(
            app.current_task_id.as_deref(),
            Some(before.as_str()),
            "same turn"
        );
    }
```

- [x] Add the new wiring tests beside them:

```rust
    /// Test 5 — D4: cancelled outranks every other route.
    #[test]
    fn cancelled_shell_output_is_never_sent() {
        for streaming in [true, false] {
            for over_budget in [true, false] {
                let route = shell::route_shell_output(true, streaming, Some("t-1"), over_budget);
                assert!(
                    matches!(route, ShellRoute::Skip(w) if w.contains("cancelled")),
                    "streaming={streaming} over_budget={over_budget}: {route:?}"
                );
            }
        }
        assert!(matches!(
            shell::route_shell_output(false, false, None, false),
            ShellRoute::Start
        ));
    }

    /// Test 6 — D3: with a shell running, Ctrl-C ends the shell and leaves
    /// the live turn alone; a second press then behaves as it always has.
    #[tokio::test]
    async fn ctrl_c_ends_the_shell_before_the_turn() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        let task = app.begin_user_turn("working");
        let (c_tx, mut c_rx) = tokio::sync::oneshot::channel();
        app.shell.begin(0, c_tx).expect("slot");

        handle_ctrl_c(&mut app, &tx);

        assert_eq!(c_rx.try_recv(), Ok(()), "the shell was signalled");
        assert!(!app.shell.is_running(), "slot freed");
        assert!(app.streaming, "the turn kept running");
        assert_eq!(app.current_task_id.as_deref(), Some(task.as_str()));

        handle_ctrl_c(&mut app, &tx);
        assert!(!app.streaming, "the second press cancelled the turn");
    }

    /// Test 17 — D11: Ctrl-C finalises the card ON THE KEYPRESS. The
    /// regression: the only event that would otherwise have stamped it is
    /// the `ShellDone` whose generation this very keypress retired, so the
    /// card stayed `streaming` forever — unstamped, unpersisted, under a
    /// footer whose ticker had also stopped.
    #[tokio::test]
    async fn ctrl_c_finalises_the_card_immediately() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        let (c_tx, _c_rx) = tokio::sync::oneshot::channel();
        let gen = app.shell.begin(4242, c_tx).expect("slot");
        app.begin_shell("sleep 60");
        app.append_shell_output("partial");

        handle_ctrl_c(&mut app, &tx);

        let card = app
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Shell)
            .expect("card");
        assert!(!card.streaming, "the spinner stopped");
        assert!(card.text.ends_with("[cancelled]"), "{}", card.text);
        assert!(!app.shell.is_running(), "no longer Ctrl-C-able");

        // The late report changes nothing further, and must not start a turn.
        let before = app.messages.len();
        handle_stream(
            &mut app,
            StreamMsg::ShellDone {
                gen,
                cmd: "sleep 60".into(),
                end: shell::ShellEnd::Cancelled,
            },
            &tx,
        );
        assert_eq!(app.messages.len(), before, "nothing more was written");
        assert!(!app.streaming, "no turn was started");
    }

    /// Test 12 — D8, one per path, each through its REAL entry point. A test
    /// that called `stop_shell` directly would have passed while any of these
    /// call sites was missing.
    #[tokio::test]
    async fn every_teardown_path_stops_the_shell_and_retires_it() {
        // (name, how the user triggers it)
        for (name, trigger) in [
            ("quit", 0u8),
            ("clear", 1u8),
            ("channel switch", 2u8),
        ] {
            let (tx, _rx) = mpsc::channel(16);
            let mut app = App::test_fixture();
            let (c_tx, mut c_rx) = tokio::sync::oneshot::channel();
            let gen = app.shell.begin(0, c_tx).expect("slot");
            app.begin_shell("sleep 60");

            match trigger {
                0 => request_quit(&mut app, &tx),
                1 => handle_slash(&mut app, SlashCmd::Clear, &tx).await,
                // The switch path: what the `app.switch_channel(&id)` call
                // site does before switching.
                _ => {
                    stop_shell(&mut app, false);
                    let _ = app.switch_channel("does-not-exist");
                }
            }

            assert!(!app.shell.is_running(), "{name}: shell still running");
            assert!(!app.shell.accepts(gen), "{name}: generation not retired");
            if trigger != 0 {
                // Quit kills the group outright; the soft paths signal the task.
                assert_eq!(c_rx.try_recv(), Ok(()), "{name}: task not signalled");
            }

            // Whatever the dying task still emits writes nothing.
            let before = app.messages.len();
            handle_stream(
                &mut app,
                StreamMsg::ShellOutput { gen, chunk: "late".into() },
                &tx,
            );
            handle_stream(
                &mut app,
                StreamMsg::ShellDone {
                    gen,
                    cmd: "sleep 60".into(),
                    end: shell::ShellEnd::Cancelled,
                },
                &tx,
            );
            assert_eq!(app.messages.len(), before, "{name}: a stale event drew");
            assert!(!app.streaming, "{name}: a stale event started a turn");
        }
    }

    /// Test 11 (wiring half) — D7: a second `!cmd` is refused with a note and
    /// the first keeps its slot.
    #[tokio::test]
    async fn a_second_bang_command_is_refused() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        let (c_tx, mut c_rx) = tokio::sync::oneshot::channel();
        app.shell.begin(0, c_tx).expect("slot");
        app.set_input("!echo two");

        submit(&mut app, &tx).await;

        assert!(app.shell.is_running(), "the first still holds the slot");
        assert!(c_rx.try_recv().is_err(), "the first was not cancelled");
        let last = app.messages.last().expect("a note");
        assert!(
            last.text.contains("already running"),
            "{}",
            last.text
        );
    }
```

  If `app.set_input` is not the composer setter used elsewhere in this test
  module, use whatever the neighbouring `submit` tests use — `grep -n "submit(&mut app" -B 4 mur-core/src/cmd/agent/cli/mod.rs` shows the setup they share.

- [x] The cutover is closed; the crate builds again. Run everything phases A and B deferred:
  `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli:: > /tmp/t1.log 2>&1; echo $?` → `0`. If `cancel_kills_the_group_promptly` fails on "outlived the cancel", `process_group(0)` is not taking effect: check it is set on the `Command` **before** `spawn()`.
- [x] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c1.log 2>&1; echo $?` → `0`. No exemptions: every file must be clean, because this is the first commit of the cutover.
- [x] Commit, once, for all three phases: `feat(murmur): !cmd streams, single-flight, cancellable to its process group (#1286 T1)`.

---

## Task 2 — `ui/message.rs`: the running footer

**Interfaces.** Consumes: `ChatMsg.streaming` on a `Role::Shell` message (T2). Produces: no new symbols.

- [x] In `push_message`'s `Role::Shell` arm, append the footer after the existing output loop:

```rust
        Role::Shell => {
            // `$ cmd` highlighted, output dim — visually a local terminal block.
            let mut it = m.text.lines();
            if let Some(first) = it.next() {
                lines.push(Line::styled(
                    first.to_string(),
                    theme.accent_alt.add_modifier(Modifier::BOLD),
                ));
            }
            for l in it {
                lines.push(Line::styled(l.to_string(), theme.muted));
            }
            // Live command: the same spinner frame the agent header uses, so
            // the two animate together, plus the one key that ends it (D3).
            // The event loop ticks this for a shell-only command too (§3.6).
            if m.streaming {
                let spin = SPINNER[spinner % SPINNER.len()];
                lines.push(Line::styled(
                    format!("{spin} running · Ctrl-C to stop"),
                    theme.muted,
                ));
            }
        }
```

`SPINNER` is already imported at the top of this file; no import change.

- [x] Add a test module at the end of `ui/message.rs`, mirroring the
  `settlement_paint_tests` module already in this file (same imports, same
  line-to-text extraction, same `&ANSI` theme and `push_message` call shape —
  `ChatMsg::for_test` is already `pub`, so nothing new is needed in `app.rs`):

```rust
#[cfg(test)]
mod shell_footer_tests {
    use super::push_message;
    use crate::cmd::agent::cli::app::{ChatMsg, Role};
    use crate::cmd::agent::cli::theme::ANSI;

    fn rendered(text: &str, streaming: bool) -> Vec<String> {
        let mut m = ChatMsg::for_test(Role::Shell, text);
        m.streaming = streaming;
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &ANSI, false, 60);
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// A live card says it is running and names the key that ends it (D3);
    /// a finished one says neither, and both keep the `$ cmd` line.
    #[test]
    fn a_running_shell_card_shows_the_footer() {
        let live = rendered("$ cargo test\nCompiling", true);
        assert!(
            live.iter().any(|l| l.contains("running · Ctrl-C to stop")),
            "{live:?}"
        );
        assert!(live.iter().any(|l| l.contains("$ cargo test")), "{live:?}");

        let done = rendered("$ cargo test\nCompiling\n[exit 0]", false);
        assert!(
            !done.iter().any(|l| l.contains("Ctrl-C to stop")),
            "{done:?}"
        );
        assert!(done.iter().any(|l| l.contains("$ cargo test")), "{done:?}");
    }
}
```

- [x] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli::ui > /tmp/t4.log 2>&1; echo $?` → `0`.
- [x] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c4.log 2>&1; echo $?` → `0`.
- [x] Commit: `feat(murmur): running footer on a live shell card (#1286 T2)`.

---

## Task 3 — Whole-crate verification, docs, PR

- [x] `grep -rn "SHELL_TIMEOUT_SECS\|push_shell\|run_local_shell" mur-core/src mur-agent-runtime/src mur-hub-gui/src-tauri/src` → **no hits**. Any remaining one is a caller the tasks missed.
- [ ] `wc -l mur-core/src/cmd/agent/cli/{shell.rs,mod.rs,app.rs,stream.rs}` — record the numbers in the PR body. `shell.rs` must be under 800; `mod.rs` and `app.rs` must be **smaller** than the 3770 / 2894 they started at, since Task 0 moved code out and Tasks 1–3 added the bulk elsewhere. If either grew, the split did not do its job and the new code is in the wrong file.
  - **Outcome: this check FAILED and the criterion was wrong, not the code.** `shell/run.rs` 584 and `shell/mod.rs` 359 are both under 800. But `mod.rs` went 3770 → 3962 and `app.rs` 2894 → 3043. The card operations mutate `App`'s transcript and call its private `persist_turn`; the wiring exercises `submit`/`handle_ctrl_c`/`handle_stream`, which live in `mod.rs`. Moving either out would mean widening `App`'s internals or making those handlers public. Both files were already 4.6x and 3.6x over the limit before this branch; bringing them under it is a refactor several times this feature's size and belongs in its own PR.
- [x] `cargo fmt --all -- --check; echo $?` → `0`.
- [x] `cargo clippy --workspace --all-targets -- -D warnings > /tmp/cw.log 2>&1; echo $?` → `0`.
- [x] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core > /tmp/tc.log 2>&1; echo $?` → `0`. ~5900 tests, about a minute after a warm build; a cold build is ~15 minutes.
- [x] `cargo nextest run -p mur-agent-runtime > /tmp/tr.log 2>&1; echo $?` → `0` (nothing here touches it; this proves it).
- [x] Hub, **last**: `cd mur-hub-gui/src-tauri && cargo check > /tmp/hub.log 2>&1; echo $?` → `0`. If the worktree lacks `mur-hub-gui/ui/dist`, symlink it from the main checkout first (`ln -s /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui/dist mur-hub-gui/ui/dist`) and **remove the symlink before committing**.
- [x] Set the spec's Status line to `Implemented in #<PR>` once the PR number exists.
- [x] Live check, recorded in the PR description (see the PR body table; all nine sub-checks observed on agent `qa` restarted alone, murmur 2.80.0). Restart ONE agent (`./install.sh`, then `mur agent restart <one agent>` — **not** `--stale`), so the rest of the machine's fleet stays on the prior build:
  - `!sleep 45` completes instead of dying at 30 s, and shows an **animating** footer the whole time (§3.6 — a frozen spinner means the tick guard is wrong).
  - `!cargo build` streams output line by line.
  - Ctrl-C during that build: the card ends `[cancelled]`, nothing is sent to the agent, `pgrep -f rustc` shows nothing from that build.
  - A second `!cmd` while one runs: refused with the note, and the first keeps running (D7).
  - With an agent turn streaming, start `!sleep 30` and press Ctrl-C: the shell ends, the turn keeps going.
  - `/clear` mid-`!cargo build`: `pgrep -f rustc` shows nothing, and the new conversation gains no stray line (D8).
  - `/channels` switch mid-`!cargo build`: the same (D8, the fourth call site).
  - Ctrl-C mid-`!cargo build`, then quit **within two seconds**: `pgrep -f rustc` shows nothing. This is the orphan the detached timer left behind (§3.5).
  - Quit (Ctrl-D) mid-`!cargo build`: `pgrep -f rustc` shows nothing (§3.5 — this is the path `kill_on_drop` alone would have left orphaned).
- [x] Open the PR (#1293): title `feat(murmur): !cmd streams and is cancelled, never killed by a clock (#1286)`, body = D1–D10 one line each, the §1.4 exit-marker fix called out as a behaviour change, the file-size numbers, and the live observations.

## Self-review

- **Spec coverage.** D1 → T1 (constant deleted, test 1). D2 → T2 (`begin_shell` at submit, test 15) + T1 (streaming, test 2) + T4 (footer). D3 → T3 (`handle_ctrl_c` first branch, test 6). D4 → T3 (`route_shell_output` first arm, test 5) + T1 (`reaches_agent`, test 10). D5 → T1 (`process_group`, `signal_group`, test 3/4) + §3.7 on Windows. D6 → T1 (`cap_tail`) + T2 (card cap, test 7) + T3 (block cap). D7 → T2 (`ShellState::begin`, test 11a) + T3 (submit guard, test 11b). D8 → T2 (`accepts`/`cancel`, test 12a) + T3 (four call sites, gen filtering, test 12b). D9 → T1 (drain before return + `DRAIN_INCOMPLETE_NOTE`, test 14). D10 → T1 (`ShellEnd`, test 10). §1.4 → T1 (`cap_tail`) + T3 (block uses it). §3.1 → T0 (movement) + T5 (file-size check). §3.5 → T2 (`cancel(hard)`) + T3 (`request_quit`) + T5 (live quit check). §3.6 → T3 (tick guard) + T5 (live animating check). §4 error table: spawn failure → T3 (submit arm) + T1 (`SpawnFailed`); group refusal → T1; second command → T3; SIGTERM ignored → T1 escalation; double Ctrl-C → T3; race on send → T2 (`let _ =`); drain bound → T1; non-UTF-8 → T1 (`pump` carry, test 9); retired generation → T3. §5 tests 1–16 all land in a task. §6 out-of-scope items are implemented nowhere.
- **Cross-task names.** `cap_tail`, `SHELL_MAX_BYTES`, `SHELL_CARD_MAX_BYTES`, `KILL_GRACE`, `DRAIN_INCOMPLETE_NOTE`, `signal_group`, `SIGTERM_NUM`/`SIGKILL_NUM`, `ShellEnd`, `spawn`, `run` (T1) are used verbatim in T2/T3. `ShellState::{is_running, generation, accepts, begin, finish}` and `shell::cancel` (T2) are used verbatim in T3. `begin_shell`/`append_shell_output`/`finish_shell` (T2) are used verbatim in T3. `route_shell_output`'s new arity (T3) is updated at every call site in the same task. `finish_shell_turn` is defined once in T3 and used by both of its callers. `ChatMsg.streaming` (existing) is set in T2 and read in T4.
- **Round-2 coverage.** D11 → Phase C (`stop_shell`) + test 17. §3.5 pid retention → Phase B (`Slot::Cancelling`, `done`) + Phase C (`ShellDone` calls `done` first) + tests 13/18 and `done_clears_a_cancelling_slot…`. Escalation in the loop → Phase A (`escalate` in the select). D6 marker-in-budget → Phase A (`cap_tail`) + three cap tests. Teardown per path → Phase C (`every_teardown_path_stops_the_shell_and_retires_it`, through `request_quit`, `handle_slash(Clear)` and the switch site) + the live check. Buildable commits → the header banner and the phase structure. Movement in its own PR → Task 0's banner and its two closing steps.
- **Known soft spots, each with an in-task instruction rather than a guess.** T2's module path to `shell::` from inside `step_app_tests` (instruction: try both). T3's composer setter in the refusal test (instruction: copy the neighbouring `submit` tests' setup). Everything the previous revision flagged in T4 was resolved before handoff: `ChatMsg::for_test` and `&theme::ANSI` exist and are used verbatim by the neighbouring module.

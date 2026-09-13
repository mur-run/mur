# Plan: murmur `!cmd` streams and is cancelled, never killed by a clock

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-13-murmur-bang-streaming-design.md` (D1–D10, §3.1–§3.7, §7 review log).
> Issue: #1286. Base: `origin/main` at or after `a78427e5`. Work in a worktree under `.worktrees/` on branch `feat/murmur-bang-streaming`.

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
- **D9:** every chunk a command will ever produce is forwarded before `ShellDone`. When a grandchild holds the pipes past the drain grace, the card says so rather than truncating silently.
- **D10:** how a command ended is one enum, `ShellEnd`, not two loose fields.
- **§1.4:** the agent block keeps the tail so `[exit N]` and a failure summary survive; `[output truncated]` leads the block.
- **§3.1 / CLAUDE.md §4:** `app.rs` is 2894 lines and `mod.rs` is 3770, both already past the 800-line rule. New code goes in `cli/shell.rs`. Task 0 is pure movement in its own commit, as the rule requires.
- stdin stays `Stdio::null()`. Interactive `!cmd` is out of scope.
- `mur-core` must not reach into `mur-agent-runtime::tools::bash_jobs` for the kill helper even though the dependency exists — `signal_group` is local to `shell.rs` (spec §3.1 records why).
- Before every commit: `cargo fmt --all`, then `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c.log 2>&1; echo $?` — read the **exit code**, never a grep of the output.

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

No behaviour change. Its own commit so a reviewer can confirm that with `git show --stat` and a diff that only moves lines. CLAUDE.md §4 asks for exactly this.

**Interfaces.** Produces: module `crate::cmd::agent::cli::shell` exporting `shell_block`, `ShellRoute`, `route_shell_output`. Consumes: nothing.

- [ ] Create `mur-core/src/cmd/agent/cli/shell.rs` containing **only** this header and the three items cut verbatim from `mod.rs` (`shell_block`, `enum ShellRoute`, `route_shell_output`, with their doc comments unchanged). `ShellRoute` and `route_shell_output` were private to `mod.rs`; they become `pub(super)`:

```rust
//! Local `!command` execution for the murmur TUI: spawning, streaming,
//! cancelling, and deciding where a finished command's output goes.
//!
//! Separate from `stream.rs`, which is the A2A streaming bridge and has
//! nothing to do with local processes, and separate from `mod.rs`, which is
//! already far past the repository's 800-line rule (CLAUDE.md §4).
```

- [ ] Delete those three items from `mod.rs` and add `mod shell;` beside its other `mod` declarations. Add `use shell::{ShellRoute, route_shell_output, shell_block};` — or qualify at the two call sites, whichever leaves `mod.rs` smaller.
- [ ] Move the three tests that cover them (`grep -n "shell_block\|route_shell_output" mur-core/src/cmd/agent/cli/mod.rs` inside the test module) into a `#[cfg(test)] mod tests` in `shell.rs`, unchanged apart from the `use super::*;` they need.
- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli:: > /tmp/t0.log 2>&1; echo $?` → `0`. Same test count as before the move; nothing was rewritten.
- [ ] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c0.log 2>&1; echo $?` → `0`.
- [ ] Commit: `refactor(murmur): move shell_block/route_shell_output into cli/shell.rs (no behaviour change) (#1286 T0)`.

---

## Task 1 — `shell.rs`: the engine

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

- [ ] In `stream.rs`, replace the `ShellDone` variant and add `ShellOutput`:

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

- [ ] In `StreamMsg::task_id`, add the new variant to the turn-independent arm:

```rust
            StreamMsg::Note(_)
            | StreamMsg::Expired { .. }
            | StreamMsg::ShellOutput { .. }
            | StreamMsg::ShellDone { .. } => None,
```

- [ ] In `stream.rs`, delete `SHELL_MAX_BYTES`, `SHELL_TIMEOUT_SECS`, the whole `run_local_shell` function, and its two tests (`run_local_shell_captures_output_and_exit`, `run_local_shell_truncates_huge_output`). Their replacements live in `shell.rs` below. `StreamMsg` must derive nothing new: confirm `#[derive(Debug)]` still compiles once `ShellEnd` derives `Debug`.

- [ ] Append to `shell.rs` (after the Task 0 items):

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

/// Keep the last `max` bytes of `text`, on a char boundary, prefixed with a
/// marker when anything was dropped.
///
/// The tail, not the head: the exit marker and a test run's verdict are the
/// last lines written, and the old head-keeping cap dropped exactly those —
/// a failing command over the cap reached the agent looking clean (§1.4).
pub fn cap_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = text.len() - max;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    format!("[output truncated]\n{}", &text[cut..])
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
                // Fire-and-forget escalation: SIGKILL to a group that already
                // died is ESRCH, which is harmless, so this needs no shared
                // "is it still alive" state.
                tokio::spawn(async move {
                    tokio::time::sleep(KILL_GRACE).await;
                    signal_group(pid, SIGKILL_NUM);
                });
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

- [ ] Add to `shell.rs`'s test module:

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

    /// Test 8 — D6 + §1.4: the cap keeps the TAIL and leads with the marker,
    /// so an exit line written last survives.
    #[test]
    fn cap_tail_keeps_the_end_and_marks_the_front() {
        let text = format!("{}\n[exit 1]", "x".repeat(100));
        let out = cap_tail(&text, 32);
        assert!(out.starts_with("[output truncated]\n"), "{out}");
        assert!(out.ends_with("[exit 1]"), "{out}");
        assert!(out.len() <= 32 + "[output truncated]\n".len());
        assert_eq!(cap_tail("short", 32), "short");
    }
```

- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli::shell > /tmp/t1.log 2>&1; echo $?` → `0`. If `cancel_kills_the_group_promptly` fails on "outlived the cancel", `process_group(0)` is not taking effect: check it is set on the `Command` **before** `spawn()`.
- [ ] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c1.log 2>&1; echo $?` → `0`. `mod.rs` and `app.rs` will be broken here (they still use the old `ShellDone` shape and `run_local_shell`); that is Tasks 2–3. Any failure *outside* those two files is real.
- [ ] Commit: `feat(murmur): shell.rs engine — streaming, process-group cancel, ShellEnd (#1286 T1)`.

---

## Task 2 — `ShellState` and the live card

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
pub fn cancel(state: &mut ShellState, hard: bool);
// app.rs
pub shell: shell::ShellState,             // App field
impl App {
    pub fn begin_shell(&mut self, cmd: &str);
    pub fn append_shell_output(&mut self, chunk: &str);
    pub fn finish_shell(&mut self, end: &shell::ShellEnd) -> String;
}
```

`push_shell` is **removed** — `begin_shell` + `finish_shell` replace it.

- [ ] Append to `shell.rs`:

```rust
/// The one running `!command`, if any (D7: single-flight), plus the
/// generation that makes its events identifiable after a teardown (D8).
#[derive(Default)]
pub struct ShellState {
    gen: u64,
    running: Option<Running>,
}

struct Running {
    pid: u32,
    cancel: oneshot::Sender<()>,
}

impl ShellState {
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn generation(&self) -> u64 {
        self.gen
    }

    /// Is an event from generation `gen` still wanted? A teardown retires the
    /// generation, so anything the dying task still emits is dropped rather
    /// than written into a cleared transcript or another channel (D8).
    pub fn accepts(&self, gen: u64) -> bool {
        gen == self.gen
    }

    /// Claim the single slot. `None` when one is already running — the caller
    /// refuses *before spawning* (D7).
    ///
    /// Never assign over a live handle: dropping an `oneshot::Sender`
    /// resolves its receiver, so an overwrite would silently cancel the
    /// command already running (§7, finding 1).
    pub fn begin(&mut self, pid: u32, cancel: oneshot::Sender<()>) -> Option<u64> {
        if self.running.is_some() {
            return None;
        }
        self.gen += 1;
        self.running = Some(Running { pid, cancel });
        Some(self.gen)
    }

    /// The command ended on its own. Frees the slot without retiring the
    /// generation — its `ShellDone` is still wanted.
    pub fn finish(&mut self, gen: u64) {
        if gen == self.gen {
            self.running = None;
        }
    }
}

/// End the running `!command`, if any, and retire its generation (D8).
///
/// `hard` is the quit path: the UI is about to stop reading, so nothing is
/// left to run the escalation timer. Signal the group directly instead —
/// dropping the task fires `kill_on_drop`, which reaches the direct shell and
/// leaves the group, i.e. exactly the orphan D5 exists to prevent (§3.5).
pub fn cancel(state: &mut ShellState, hard: bool) {
    let Some(run) = state.running.take() else {
        return;
    };
    state.gen += 1;
    if hard {
        // No grace: there is no one left to wait for it. TERM gives a
        // fast-handling child its chance; KILL guarantees the rest.
        signal_group(run.pid, SIGTERM_NUM);
        signal_group(run.pid, SIGKILL_NUM);
    } else {
        // Err = the command already exited and the receiver is gone.
        let _ = run.cancel.send(());
    }
}
```

- [ ] Add to `shell.rs`'s tests:

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
        cancel(&mut s, false);
        assert!(!s.accepts(gen), "stale events are rejected");
        assert!(!s.is_running());
        assert_eq!(rx.try_recv(), Ok(()), "the soft path signalled the task");
        // Idempotent: a second press has nothing to take.
        cancel(&mut s, false);
    }

    /// A natural end frees the slot but keeps the generation, because its own
    /// `ShellDone` still has to be accepted.
    #[test]
    fn finish_frees_the_slot_without_retiring_the_generation() {
        let mut s = ShellState::default();
        let (tx, _rx) = oneshot::channel();
        let gen = s.begin(0, tx).unwrap();
        s.finish(gen);
        assert!(!s.is_running());
        assert!(s.accepts(gen), "its own ShellDone is still wanted");
    }
```

- [ ] In `app.rs`, add the field immediately after `pub ctrl_c_hint: bool,`:

```rust
    /// The running `!cmd`, if any, and its generation. `is_running()` is what
    /// makes Ctrl-C end the shell rather than the agent turn (D3), and what
    /// keeps the spinner ticking for a shell-only command (§3.6).
    pub shell: super::shell::ShellState,
```

- [ ] Add `shell: Default::default(),` to the single `App` construction site, immediately after `ctrl_c_hint: false,` (grep `ctrl_c_hint:` — exactly two hits, the field and this one).

- [ ] Replace the whole `push_shell` method with these three:

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

- [ ] Add to `app.rs`'s `step_app_tests` module (it already has `use super::*;` and a local `fn app() -> App`). If `super::shell::` does not resolve from inside that module, use `crate::cmd::agent::cli::shell::` — both name the same module; pick whichever compiles and use it consistently:

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

- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli::shell cli::app > /tmp/t2.log 2>&1; echo $?` → `0`.
- [ ] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c2.log 2>&1; echo $?` → `0`. `mod.rs` is still broken here — Task 3. Any failure outside `mod.rs` is real.
- [ ] Commit: `feat(murmur): single-flight ShellState with generations + live card ops (#1286 T2)`.

---

## Task 3 — `mod.rs`: wiring, the four teardown sites, the spinner

**Interfaces.** Consumes everything from T1 and T2. Produces: `route_shell_output(cancelled, streaming, task_id, over_budget)`.

- [ ] In `shell.rs`, give the moved `route_shell_output` its new leading parameter and first arm:

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

- [ ] In `submit`, replace the `!command` arm's body:

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

- [ ] Add the shared finaliser beside `start_shell_turn` (used by both the spawn-failure path above and the `ShellDone` arm below, so the routing rule exists once):

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

- [ ] Replace the `ShellDone` arm of `handle_stream` and add a `ShellOutput` arm beside it:

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
            if !app.shell.accepts(gen) {
                return;
            }
            app.shell.finish(gen);
            let output = app.finish_shell(&end);
            finish_shell_turn(app, &cmd, &end, output, tx);
        }
```

- [ ] In `handle_ctrl_c`, add a new **first** branch, above `if app.streaming`:

```rust
fn handle_ctrl_c(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    // D3: a running `!cmd` is what Ctrl-C ends — it is the thing the user
    // just launched and is watching. Any agent turn keeps running and still
    // has Esc-Esc. The state is retired here, so a second press falls
    // through to the behaviour below, unchanged.
    if app.shell.is_running() {
        shell::cancel(&mut app.shell, false);
        return;
    }
    if app.streaming {
        // … unchanged …
```

- [ ] Add the remaining three teardown sites (D8). In `request_quit`, before `app.should_quit = true;`:

```rust
    // Hard: the event loop is about to stop, so nothing is left to run the
    // escalation timer, and dropping the task would only `kill_on_drop` the
    // direct shell and leave its group (§3.5).
    shell::cancel(&mut app.shell, true);
```

  In the `SlashCmd::Clear` arm, immediately after the existing
  `cancel_in_flight(app, tx);` — the comment already there ("so its worker
  can't write into the fresh conversation after the reset") is the same
  reason, so put it under the same one:

```rust
            shell::cancel(&mut app.shell, false);
```

  At the `app.switch_channel(&id)` call site (`mod.rs:2105`), immediately
  before the call:

```rust
                        shell::cancel(&mut app.shell, false);
```

- [ ] Change the event loop's spinner guard (`mod.rs:1039`) so a shell-only command animates (§3.6) — without this the footer renders once and freezes, which reads as hung:

```rust
            _ = spinner.tick(), if app.streaming || app.shell.is_running() => app.tick_spinner(),
```

- [ ] The two existing tests `shell_done_while_idle_starts_a_turn_without_a_user_bubble` and `shell_done_while_streaming_steers_the_live_turn` construct `StreamMsg::ShellDone { cmd, output }`, which no longer exists. Rewrite their setup — the assertions that follow are unchanged:

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

- [ ] Add the new wiring tests beside them:

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

    /// Test 12 — D8: after a teardown, the dying task's events are dropped.
    /// No card, no persisted turn, no stray system note in the conversation
    /// the user moved on to.
    #[tokio::test]
    async fn events_from_a_retired_generation_are_dropped() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        let (c_tx, _c_rx) = tokio::sync::oneshot::channel();
        let gen = app.shell.begin(0, c_tx).expect("slot");
        app.begin_shell("sleep 60");

        // What every teardown path does.
        shell::cancel(&mut app.shell, false);
        app.messages.clear();
        let before = app.messages.len();

        handle_stream(&mut app, StreamMsg::ShellOutput { gen, chunk: "late".into() }, &tx);
        handle_stream(
            &mut app,
            StreamMsg::ShellDone {
                gen,
                cmd: "sleep 60".into(),
                end: shell::ShellEnd::Cancelled,
            },
            &tx,
        );

        assert_eq!(app.messages.len(), before, "nothing was written");
        assert!(!app.streaming, "no turn was started");
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

- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli:: > /tmp/t3.log 2>&1; echo $?` → `0`.
- [ ] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c3.log 2>&1; echo $?` → `0`.
- [ ] Commit: `feat(murmur): single-flight wiring, four teardown sites, shell-aware spinner (#1286 T3)`.

---

## Task 4 — `ui/message.rs`: the running footer

**Interfaces.** Consumes: `ChatMsg.streaming` on a `Role::Shell` message (T2). Produces: no new symbols.

- [ ] In `push_message`'s `Role::Shell` arm, append the footer after the existing output loop:

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

- [ ] Add a test module at the end of `ui/message.rs`, mirroring the
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

- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli::ui > /tmp/t4.log 2>&1; echo $?` → `0`.
- [ ] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c4.log 2>&1; echo $?` → `0`.
- [ ] Commit: `feat(murmur): running footer on a live shell card (#1286 T4)`.

---

## Task 5 — Whole-crate verification, docs, PR

- [ ] `grep -rn "SHELL_TIMEOUT_SECS\|push_shell\|run_local_shell" mur-core/src mur-agent-runtime/src mur-hub-gui/src-tauri/src` → **no hits**. Any remaining one is a caller the tasks missed.
- [ ] `wc -l mur-core/src/cmd/agent/cli/{shell.rs,mod.rs,app.rs,stream.rs}` — record the numbers in the PR body. `shell.rs` must be under 800; `mod.rs` and `app.rs` must be **smaller** than the 3770 / 2894 they started at, since Task 0 moved code out and Tasks 1–3 added the bulk elsewhere. If either grew, the split did not do its job and the new code is in the wrong file.
- [ ] `cargo fmt --all -- --check; echo $?` → `0`.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings > /tmp/cw.log 2>&1; echo $?` → `0`.
- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core > /tmp/tc.log 2>&1; echo $?` → `0`. ~5900 tests, about a minute after a warm build; a cold build is ~15 minutes.
- [ ] `cargo nextest run -p mur-agent-runtime > /tmp/tr.log 2>&1; echo $?` → `0` (nothing here touches it; this proves it).
- [ ] Hub, **last**: `cd mur-hub-gui/src-tauri && cargo check > /tmp/hub.log 2>&1; echo $?` → `0`. If the worktree lacks `mur-hub-gui/ui/dist`, symlink it from the main checkout first (`ln -s /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui/dist mur-hub-gui/ui/dist`) and **remove the symlink before committing**.
- [ ] Set the spec's Status line to `Implemented in #<PR>` once the PR number exists.
- [ ] Live check, recorded in the PR description. Restart ONE agent (`./install.sh`, then `mur agent restart <one agent>` — **not** `--stale`), so the rest of the machine's fleet stays on the prior build:
  - `!sleep 45` completes instead of dying at 30 s, and shows an **animating** footer the whole time (§3.6 — a frozen spinner means the tick guard is wrong).
  - `!cargo build` streams output line by line.
  - Ctrl-C during that build: the card ends `[cancelled]`, nothing is sent to the agent, `pgrep -f rustc` shows nothing from that build.
  - A second `!cmd` while one runs: refused with the note, and the first keeps running (D7).
  - With an agent turn streaming, start `!sleep 30` and press Ctrl-C: the shell ends, the turn keeps going.
  - `/clear` mid-`!cargo build`: `pgrep -f rustc` shows nothing, and the new conversation gains no stray line (D8).
  - Quit (Ctrl-D) mid-`!cargo build`: `pgrep -f rustc` shows nothing (§3.5 — this is the path `kill_on_drop` alone would have left orphaned).
- [ ] Open the PR: title `feat(murmur): !cmd streams and is cancelled, never killed by a clock (#1286)`, body = D1–D10 one line each, the §1.4 exit-marker fix called out as a behaviour change, the file-size numbers, and the live observations.

## Self-review

- **Spec coverage.** D1 → T1 (constant deleted, test 1). D2 → T2 (`begin_shell` at submit, test 15) + T1 (streaming, test 2) + T4 (footer). D3 → T3 (`handle_ctrl_c` first branch, test 6). D4 → T3 (`route_shell_output` first arm, test 5) + T1 (`reaches_agent`, test 10). D5 → T1 (`process_group`, `signal_group`, test 3/4) + §3.7 on Windows. D6 → T1 (`cap_tail`) + T2 (card cap, test 7) + T3 (block cap). D7 → T2 (`ShellState::begin`, test 11a) + T3 (submit guard, test 11b). D8 → T2 (`accepts`/`cancel`, test 12a) + T3 (four call sites, gen filtering, test 12b). D9 → T1 (drain before return + `DRAIN_INCOMPLETE_NOTE`, test 14). D10 → T1 (`ShellEnd`, test 10). §1.4 → T1 (`cap_tail`) + T3 (block uses it). §3.1 → T0 (movement) + T5 (file-size check). §3.5 → T2 (`cancel(hard)`) + T3 (`request_quit`) + T5 (live quit check). §3.6 → T3 (tick guard) + T5 (live animating check). §4 error table: spawn failure → T3 (submit arm) + T1 (`SpawnFailed`); group refusal → T1; second command → T3; SIGTERM ignored → T1 escalation; double Ctrl-C → T3; race on send → T2 (`let _ =`); drain bound → T1; non-UTF-8 → T1 (`pump` carry, test 9); retired generation → T3. §5 tests 1–16 all land in a task. §6 out-of-scope items are implemented nowhere.
- **Cross-task names.** `cap_tail`, `SHELL_MAX_BYTES`, `SHELL_CARD_MAX_BYTES`, `KILL_GRACE`, `DRAIN_INCOMPLETE_NOTE`, `signal_group`, `SIGTERM_NUM`/`SIGKILL_NUM`, `ShellEnd`, `spawn`, `run` (T1) are used verbatim in T2/T3. `ShellState::{is_running, generation, accepts, begin, finish}` and `shell::cancel` (T2) are used verbatim in T3. `begin_shell`/`append_shell_output`/`finish_shell` (T2) are used verbatim in T3. `route_shell_output`'s new arity (T3) is updated at every call site in the same task. `finish_shell_turn` is defined once in T3 and used by both of its callers. `ChatMsg.streaming` (existing) is set in T2 and read in T4.
- **Known soft spots, each with an in-task instruction rather than a guess.** T2's module path to `shell::` from inside `step_app_tests` (instruction: try both). T3's composer setter in the refusal test (instruction: copy the neighbouring `submit` tests' setup). Everything the previous revision flagged in T4 was resolved before handoff: `ChatMsg::for_test` and `&theme::ANSI` exist and are used verbatim by the neighbouring module.

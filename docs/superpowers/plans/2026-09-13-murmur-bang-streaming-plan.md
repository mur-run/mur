# Plan: murmur `!cmd` streams and is cancelled, never killed by a clock

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-13-murmur-bang-streaming-design.md` (D1–D6 and §1.4).
> Issue: #1286. Base: `origin/main` at or after `a78427e5`. Work in a worktree under `.worktrees/` on branch `feat/murmur-bang-streaming`.

**Goal.** A `!cmd` in murmur runs with no wall-clock limit, streams its output into the transcript as it arrives, and ends only when it finishes or the user presses Ctrl-C — which kills its whole process group and does not wake the agent.

**Architecture.** `run_local_shell` (buffer-everything + 30 s `tokio::time::timeout`) is replaced by `run_local_shell_streaming`, which spawns the shell as its own process group, pumps stdout and stderr through per-stream UTF-8 carries into `StreamMsg::ShellOutput` chunks, and selects on a `oneshot` cancel that signals the group. The UI accumulates those chunks into one live `Role::Shell` card (`ChatMsg.streaming`, the same flag agent turns already use), and `handle_ctrl_c` consults a new `App::shell_cancel` before anything else, so the shell — not the agent turn — is what a Ctrl-C ends.

**Tech stack.** Rust 2024, `cargo nextest`. `mur-core` env: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`. `libc` is already a `mur-core` dependency (`mur-core/Cargo.toml:42`) — do not add it.

## Global Constraints (from the spec)

- **D1:** delete `SHELL_TIMEOUT_SECS`. Do not raise it, do not replace it with a larger constant, do not add a configurable one. Ctrl-C is the only bound.
- **D2:** output streams — each read forwards a `StreamMsg::ShellOutput` before the command ends.
- **D3:** Ctrl-C ends the shell and **only** the shell. With no `!cmd` running, `handle_ctrl_c` behaves exactly as it does today and its existing tests must keep passing unchanged.
- **D4:** a cancelled command's output is never sent to the agent — no turn, no steer. Its card stays in the transcript.
- **D5:** the child is spawned with `process_group(0)` on unix; the kill is `killpg(SIGTERM)` then `killpg(SIGKILL)` after `KILL_GRACE` (2 s). A `process_group` failure fails the spawn — never fall back to an ungrouped child.
- **D6:** two caps, both keeping the **tail**: the card at `SHELL_CARD_MAX_BYTES` (256 KiB), the agent block at `SHELL_MAX_BYTES` (8 KiB, value unchanged).
- **§1.4:** the agent block keeps the tail so `[exit N]` and a failure summary survive truncation; `[output truncated]` moves to the **front** of the block.
- stdin stays `Stdio::null()`. Interactive `!cmd` is out of scope.
- `mur-core` must not reach into `mur-agent-runtime::tools::bash_jobs` for the kill helper even though the dependency exists — `signal_group` is a local helper in `stream.rs` (spec §3.2 records why).
- Before every commit: `cargo fmt --all`, then `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c.log 2>&1; echo $?` — read the **exit code**, never a grep of the output.
- `mur-hub-gui` does not reference any symbol in `cli/`; no Hub change is expected. Task 5 verifies this rather than assuming it.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-core/src/cmd/agent/cli/stream.rs` | `cap_tail`, `signal_group`, `ShellOutcome`, `run_local_shell_streaming`, the two `StreamMsg` variants | 1 |
| `mur-core/src/cmd/agent/cli/app.rs` | `shell_cancel` field; `begin_shell` / `append_shell_output` / `finish_shell` replacing `push_shell` | 2 |
| `mur-core/src/cmd/agent/cli/mod.rs` | `submit` wiring, `handle_ctrl_c` precedence, `handle_stream` arms, `route_shell_output` cancelled arm, block tail-cap | 3 |
| `mur-core/src/cmd/agent/cli/ui/message.rs` | the running footer on a streaming Shell card | 4 |

---

## Task 1 — `stream.rs`: the streaming, cancellable, group-killing engine

**Interfaces.** Produces, all in `crate::cmd::agent::cli::stream`:

```rust
pub const SHELL_MAX_BYTES: usize = 8 * 1024;        // unchanged value
pub const SHELL_CARD_MAX_BYTES: usize = 256 * 1024; // new
pub const KILL_GRACE: Duration = Duration::from_secs(2);
pub fn cap_tail(text: &str, max: usize) -> String;
pub struct ShellOutcome { pub exit: Option<i32>, pub cancelled: bool }
pub async fn run_local_shell_streaming(
    cmd: String,
    tx: mpsc::Sender<StreamMsg>,
    cancel: oneshot::Receiver<()>,
) -> ShellOutcome;
// StreamMsg gains:
//   ShellOutput { chunk: String }
//   ShellDone   { cmd: String, exit: Option<i32>, cancelled: bool }   (shape CHANGED)
```

Consumes: nothing.

- [ ] In `stream.rs`, extend the imports at the top of the file (it already has `use tokio::sync::mpsc;`):

```rust
use tokio::io::AsyncReadExt;
use tokio::sync::oneshot;
```

- [ ] Replace the `ShellDone` variant in `enum StreamMsg` and add `ShellOutput` beside it:

```rust
    /// A chunk of a running local `!command`'s output (stdout and stderr
    /// interleaved in arrival order, as a terminal shows them).
    /// Turn-independent like `Note`.
    ShellOutput { chunk: String },
    /// A local `!command` ended. `cancelled` means the user pressed Ctrl-C,
    /// which (D4) means its output must not be sent to the agent. The output
    /// itself is not here: it was streamed, and the live card owns it.
    ShellDone {
        cmd: String,
        exit: Option<i32>,
        cancelled: bool,
    },
```

- [ ] In `StreamMsg::task_id`, add the new variant to the turn-independent arm:

```rust
            StreamMsg::Note(_)
            | StreamMsg::Expired { .. }
            | StreamMsg::ShellOutput { .. }
            | StreamMsg::ShellDone { .. } => None,
```

- [ ] Replace the whole block from `/// Cap on captured …` through the end of `run_local_shell` (the function ending `text.trim_end().to_string()`) with:

```rust
/// Cap on the `!command` block forwarded to the agent. Value unchanged from
/// the buffered implementation; what changed is which end survives (§1.4).
pub const SHELL_MAX_BYTES: usize = 8 * 1024;
/// Cap on what one `!command` card keeps in the transcript. Far larger than
/// the agent's block — a human scrolls, a model pays per token — but not
/// unbounded: `!yes` would otherwise grow the TUI without end (D6).
pub const SHELL_CARD_MAX_BYTES: usize = 256 * 1024;
/// SIGTERM → this → SIGKILL, on the whole group (D5).
pub const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
/// After the child exits, how long to keep draining its pipes. A grandchild
/// can hold them open past the shell's own exit, so this must be bounded or
/// a `!cmd` that spawned a daemon would never finish.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(250);
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

/// How a `!command` ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOutcome {
    /// `None` when a signal ended it or the status carried no code.
    pub exit: Option<i32>,
    /// The user pressed Ctrl-C (D4: do not wake the agent).
    pub cancelled: bool,
}

/// SIGTERM/SIGKILL a whole process group.
///
/// Deliberately a local copy rather than `mur-agent-runtime`'s: the shared
/// part is three libc calls, the surrounding shapes differ (a job table and
/// a watch channel there, one child and a oneshot here), and making that
/// crate's private helper public would couple this TUI to the agent's
/// tool-execution internals. The group exists at all for the reason recorded
/// as D9 in the 2026-09-12 bash-yield spec: killing the shell alone leaves
/// `cargo`'s `rustc` children running.
#[cfg(unix)]
fn signal_group(pid: u32, sig: i32) {
    // SAFETY: `pid` came from `Child::id()` of a child spawned with
    // `process_group(0)`, so it is also the group id. `killpg` on a raw pgid
    // has no memory-safety hazard; a group that is already gone gives ESRCH.
    unsafe {
        libc::killpg(pid as libc::pid_t, sig);
    }
}

#[cfg(not(unix))]
fn signal_group(pid: u32, _sig: i32) {
    // ponytail: taskkill /T kills the tree on Windows; Job Objects if a
    // Windows user ever reports an orphan this misses.
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Read one pipe to EOF, forwarding decoded text.
///
/// The decode is per read, and a read boundary is chosen by the kernel, so a
/// multi-byte character can straddle two reads. `carry` holds an incomplete
/// trailing sequence back until the next read rather than emitting a
/// replacement character for a character that is perfectly fine.
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

/// Run a local `!command` through the user's shell (`$SHELL -c`, fallback
/// `/bin/sh`; `cmd /C` on Windows), streaming its output to the UI as
/// `StreamMsg::ShellOutput`.
///
/// Not bounded by any clock (D1): it returns when the command finishes or
/// when `cancel` fires, and nothing else. The text is not returned — it was
/// streamed, and the live card owns it.
pub async fn run_local_shell_streaming(
    cmd: String,
    tx: mpsc::Sender<StreamMsg>,
    cancel: oneshot::Receiver<()>,
) -> ShellOutcome {
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
        .arg(&cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // D5: its own group, so Ctrl-C reaches `cargo`'s children and not just
    // the shell. A refusal fails the spawn below — never an ungrouped child,
    // which would make Ctrl-C lie about what it ended.
    #[cfg(unix)]
    command.process_group(0);

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx
                .send(StreamMsg::ShellOutput {
                    chunk: format!("[failed to run: {e}]"),
                })
                .await;
            return ShellOutcome {
                exit: None,
                cancelled: false,
            };
        }
    };
    // Captured before the child moves into the waiter task: the kill goes by
    // pid, so the handle is not needed for it.
    let pid = child.id().unwrap_or(0);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // One channel, two readers. Both clones must be moved into the tasks so
    // that EOF on both pipes closes the channel — a stray clone here would
    // make the drain below wait out its whole grace period every time.
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    if let Some(o) = stdout {
        tokio::spawn(pump(o, out_tx.clone()));
    }
    if let Some(e) = stderr {
        tokio::spawn(pump(e, out_tx));
    } else {
        drop(out_tx);
    }

    let (done_tx, mut done_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = done_tx.send(child.wait().await);
    });

    let mut cancel = cancel;
    let mut cancelled = false;
    let mut exit: Option<i32> = None;
    loop {
        tokio::select! {
            Some(chunk) = out_rx.recv() => {
                if tx.send(StreamMsg::ShellOutput { chunk }).await.is_err() {
                    break; // UI gone
                }
            }
            r = &mut done_rx => {
                exit = r.ok().and_then(|s| s.ok()).and_then(|s| s.code());
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

    // The child is gone but a grandchild may still hold the pipes, so the
    // drain is bounded (the same reason the runtime's job pump bounds its).
    let _ = tokio::time::timeout(DRAIN_GRACE, async {
        while let Some(chunk) = out_rx.recv().await {
            if tx.send(StreamMsg::ShellOutput { chunk }).await.is_err() {
                return;
            }
        }
    })
    .await;

    ShellOutcome { exit, cancelled }
}

#[cfg(unix)]
const SIGTERM_NUM: i32 = libc::SIGTERM;
#[cfg(unix)]
const SIGKILL_NUM: i32 = libc::SIGKILL;
#[cfg(not(unix))]
const SIGTERM_NUM: i32 = 0;
#[cfg(not(unix))]
const SIGKILL_NUM: i32 = 0;
```

- [ ] Delete the two now-stale tests in `stream.rs`'s test module —
  `run_local_shell_captures_output_and_exit` and
  `run_local_shell_truncates_huge_output` — and add these in their place
  (they exercise the same behaviours through the new entry point plus the
  three new ones):

```rust
    /// Drive a command to completion, collecting every streamed chunk.
    #[cfg(unix)]
    async fn drive(cmd: &str) -> (String, ShellOutcome) {
        let (tx, mut rx) = mpsc::channel(256);
        let (_c_tx, c_rx) = oneshot::channel();
        let outcome = run_local_shell_streaming(cmd.into(), tx, c_rx).await;
        let mut text = String::new();
        while let Ok(m) = rx.try_recv() {
            if let StreamMsg::ShellOutput { chunk } = m {
                text.push_str(&chunk);
            }
        }
        (text, outcome)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn streams_output_and_reports_exit() {
        let (text, outcome) = drive("echo hi; echo err >&2; exit 3").await;
        assert!(text.contains("hi"), "{text}");
        assert!(text.contains("err"), "{text}");
        assert_eq!(outcome.exit, Some(3));
        assert!(!outcome.cancelled);
    }

    /// Test 1 — D1: a command that outlives the deleted 30 s ceiling still
    /// finishes on its own terms. `SHELL_TIMEOUT_SECS` is gone, so the real
    /// assertion is that nothing but the command ends the call.
    #[cfg(unix)]
    #[tokio::test]
    async fn no_clock_ends_a_slow_command() {
        let t0 = std::time::Instant::now();
        let (text, outcome) = drive("sleep 1.5; echo late").await;
        assert!(t0.elapsed() >= std::time::Duration::from_millis(1400));
        assert!(text.contains("late"), "{text}");
        assert_eq!(outcome.exit, Some(0));
    }

    /// Test 2 — D2: output arrives in more than one chunk BEFORE the command
    /// ends. The assertion that separates streaming from buffering.
    #[cfg(unix)]
    #[tokio::test]
    async fn output_arrives_before_the_command_ends() {
        let (tx, mut rx) = mpsc::channel(256);
        let (_c_tx, c_rx) = oneshot::channel();
        let task = tokio::spawn(run_local_shell_streaming(
            "echo one; sleep 0.6; echo two".into(),
            tx,
            c_rx,
        ));
        // Read the first chunk while the command is still sleeping.
        let first = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("a chunk before the command finished")
            .expect("channel open");
        assert!(!task.is_finished(), "the command already ended: not streaming");
        match first {
            StreamMsg::ShellOutput { chunk } => assert!(chunk.contains("one"), "{chunk}"),
            other => panic!("expected ShellOutput, got {other:?}"),
        }
        let outcome = task.await.unwrap();
        assert_eq!(outcome.exit, Some(0));
    }

    /// Tests 3 + 4 — D5: cancel kills the whole group (the grandchild dies,
    /// not just the shell) and returns promptly rather than after the
    /// command's natural duration.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_the_group_promptly() {
        let (tx, mut rx) = mpsc::channel(256);
        let (c_tx, c_rx) = oneshot::channel();
        let task = tokio::spawn(run_local_shell_streaming(
            "sleep 60 & echo $!; wait".into(),
            tx,
            c_rx,
        ));
        let chunk = loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("grandchild pid line")
                .expect("channel open")
            {
                StreamMsg::ShellOutput { chunk } if !chunk.trim().is_empty() => break chunk,
                _ => continue,
            }
        };
        let grandchild: u32 = chunk.trim().parse().expect("a pid on stdout");
        assert!(alive(grandchild), "grandchild should be running");

        let t0 = std::time::Instant::now();
        c_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(KILL_GRACE * 3, task)
            .await
            .expect("cancel returned promptly, not after the 60s sleep")
            .unwrap();
        assert!(outcome.cancelled);
        assert!(t0.elapsed() < KILL_GRACE * 3);

        // The regression this exists for: killing the shell alone leaves the
        // grandchild running.
        let deadline = std::time::Instant::now() + KILL_GRACE + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline && alive(grandchild) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(!alive(grandchild), "grandchild {grandchild} outlived the cancel");
    }

    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 probes existence only.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// Test 9 — a multi-byte character split across two reads is decoded
    /// once, not as two replacement characters.
    #[cfg(unix)]
    #[tokio::test]
    async fn utf8_split_across_reads_survives() {
        // Two writes with a pause, splitting the 3-byte ✓ (E2 9C 93).
        let (text, _) = drive("printf '\\xe2'; sleep 0.3; printf '\\x9c\\x93'").await;
        assert_eq!(text, "✓", "got {text:?}");
    }

    /// Test 7/8 — D6 + §1.4: the cap keeps the TAIL and leads with the
    /// marker, so an exit line written last survives.
    #[test]
    fn cap_tail_keeps_the_end_and_marks_the_front() {
        let text = format!("{}\n[exit 1]", "x".repeat(100));
        let out = cap_tail(&text, 32);
        assert!(out.starts_with("[output truncated]\n"), "{out}");
        assert!(out.ends_with("[exit 1]"), "{out}");
        assert!(out.len() <= 32 + "[output truncated]\n".len());
        // Under the cap, untouched.
        assert_eq!(cap_tail("short", 32), "short");
    }
```

- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli::stream > /tmp/t1.log 2>&1; echo $?` → `0`. If `cancel_kills_the_group_promptly` fails on "outlived the cancel", `process_group(0)` is not taking effect: check it is set on the `Command` **before** `spawn()`.
- [ ] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c1.log 2>&1; echo $?` → `0`.
- [ ] Commit: `feat(murmur): !cmd streams, is cancellable, and kills its process group (#1286 T1)`.

---

## Task 2 — `app.rs`: the live shell card and the cancel handle

**Interfaces.** Consumes: `SHELL_CARD_MAX_BYTES`, `cap_tail` (T1). Produces:

```rust
pub shell_cancel: Option<tokio::sync::oneshot::Sender<()>>,   // App field
impl App {
    pub fn begin_shell(&mut self, cmd: &str);
    pub fn append_shell_output(&mut self, chunk: &str);
    /// Returns the output body (card text minus its `$ cmd` first line) for
    /// the agent block.
    pub fn finish_shell(&mut self, exit: Option<i32>, cancelled: bool) -> String;
}
```

`push_shell` is **removed** — `begin_shell` + `finish_shell` replace it.

- [ ] Add the field to `App`, immediately after `pub ctrl_c_hint: bool,`:

```rust
    /// Cancel handle for the `!cmd` in flight, if any. `Some` is what makes
    /// Ctrl-C end the shell rather than the agent turn (D3); `handle_ctrl_c`
    /// takes it, so a second press falls through to the normal behaviour.
    pub shell_cancel: Option<tokio::sync::oneshot::Sender<()>>,
```

- [ ] Add `shell_cancel: None,` to the single `App` construction site, immediately after `ctrl_c_hint: false,` (grep `ctrl_c_hint:` — exactly two hits, the field and this one).

- [ ] Replace the whole `push_shell` method with the three below:

```rust
    /// Open the live card for a `!cmd` that just started. The card exists
    /// from the keypress, so the user sees the command echoed before any
    /// output arrives, and `append_shell_output` always has a target.
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
            return; // no live card: a late chunk after finalise; drop it
        };
        if !m.text.ends_with('\n') && !m.text.is_empty() {
            m.text.push('\n');
        }
        m.text.push_str(chunk);
        if m.text.len() > super::stream::SHELL_CARD_MAX_BYTES {
            // The `$ cmd` line is the card's identity; keep it above the
            // truncation marker rather than letting the tail eat it.
            let first = m.text.lines().next().unwrap_or_default().to_string();
            let rest = m.text.split_once('\n').map(|(_, r)| r).unwrap_or_default();
            let kept = super::stream::cap_tail(rest, super::stream::SHELL_CARD_MAX_BYTES);
            m.text = format!("{first}\n{kept}");
        }
    }

    /// Close the live `!cmd` card: stamp how it ended, stop the spinner,
    /// persist it, and hand back the output body for the agent block.
    pub fn finish_shell(&mut self, exit: Option<i32>, cancelled: bool) -> String {
        let Some(m) = self.streaming_shell_mut() else {
            return String::new();
        };
        let tail = if cancelled {
            Some("[cancelled]".to_string())
        } else {
            match exit {
                Some(0) | None => None,
                Some(code) => Some(format!("[exit {code}]")),
            }
        };
        if let Some(t) = tail {
            if !m.text.ends_with('\n') {
                m.text.push('\n');
            }
            m.text.push_str(&t);
        }
        m.streaming = false;
        let text = m.text.trim_end().to_string();
        m.text = text.clone();
        self.persist_turn("shell", &text, None, &[]);
        // The block wants the output alone; the card's first line is `$ cmd`
        // and `shell_block` re-adds it.
        text.split_once('\n').map(|(_, r)| r.to_string()).unwrap_or_default()
    }
```

- [ ] Add tests at the end of `app.rs`'s `step_app_tests` module (it already has `use super::*;` and a local `fn app() -> App`):

```rust
    /// Test 10 — the card opens on the keypress, accumulates, and stamps a
    /// non-zero exit; a clean exit stamps nothing.
    #[test]
    fn shell_card_opens_accumulates_and_stamps_exit() {
        let mut a = app();
        a.begin_shell("cargo test");
        let card = a.messages.last().expect("card");
        assert_eq!(card.text, "$ cargo test");
        assert!(card.streaming, "the card is live");

        a.append_shell_output("running 3 tests");
        a.append_shell_output("test result: FAILED");
        let body = a.finish_shell(Some(1), false);
        let card = a.messages.last().expect("card");
        assert!(!card.streaming, "the card is finalised");
        assert_eq!(card.text, "$ cargo test\nrunning 3 tests\ntest result: FAILED\n[exit 1]");
        assert_eq!(body, "running 3 tests\ntest result: FAILED\n[exit 1]");

        let mut b = app();
        b.begin_shell("true");
        b.append_shell_output("ok");
        b.finish_shell(Some(0), false);
        assert_eq!(b.messages.last().unwrap().text, "$ true\nok");
    }

    /// D4 render half: a cancelled card says so.
    #[test]
    fn cancelled_shell_card_is_marked() {
        let mut a = app();
        a.begin_shell("sleep 60");
        a.append_shell_output("partial");
        let body = a.finish_shell(None, true);
        assert_eq!(a.messages.last().unwrap().text, "$ sleep 60\npartial\n[cancelled]");
        assert!(body.contains("[cancelled]"));
    }

    /// Test 7 — D6: the card cap keeps the tail and never eats `$ cmd`.
    #[test]
    fn shell_card_cap_keeps_the_tail_and_the_command_line() {
        let mut a = app();
        a.begin_shell("noisy");
        a.append_shell_output(&"x".repeat(super::super::stream::SHELL_CARD_MAX_BYTES + 1024));
        a.append_shell_output("LAST");
        let text = &a.messages.last().unwrap().text;
        assert!(text.starts_with("$ noisy\n"), "command line survived");
        assert!(text.ends_with("LAST"), "tail survived");
        assert!(text.contains("[output truncated]"));
        assert!(text.len() < super::super::stream::SHELL_CARD_MAX_BYTES + 512);
    }

    /// An empty-output command still leaves exactly one card.
    #[test]
    fn empty_output_still_leaves_one_card() {
        let mut a = app();
        a.begin_shell("true");
        a.finish_shell(Some(0), false);
        assert_eq!(
            a.messages.iter().filter(|m| m.role == Role::Shell).count(),
            1
        );
        assert_eq!(a.messages.last().unwrap().text, "$ true");
    }
```

If the `super::super::stream::` path does not resolve from inside
`step_app_tests`, use `crate::cmd::agent::cli::stream::` instead — both name
the same module; pick whichever compiles and use it consistently.

- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli::app > /tmp/t2.log 2>&1; echo $?` → `0`.
- [ ] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c2.log 2>&1; echo $?` → `0`. Expect `mod.rs` to be **broken** at this point (it still calls `push_shell` and the old `ShellDone` shape) — that is Task 3. If clippy fails only inside `mod.rs` on those two symbols, that is expected; commit and continue. Any other failure is real.
- [ ] Commit: `feat(murmur): live shell card + cancel handle on App (#1286 T2)`.

---

## Task 3 — `mod.rs`: wiring, Ctrl-C precedence, and the cancelled route

**Interfaces.** Consumes: `run_local_shell_streaming`, `ShellOutcome`, `cap_tail`, `SHELL_MAX_BYTES`, the new `StreamMsg` variants (T1); `begin_shell`, `append_shell_output`, `finish_shell`, `shell_cancel` (T2). Produces: `route_shell_output(cancelled, streaming, task_id, over_budget)`.

- [ ] In `submit`, replace the `!command` arm's body:

```rust
    if let Some(cmd) = trimmed.strip_prefix('!').map(str::trim)
        && !cmd.is_empty()
    {
        app.clear_input();
        // No "running …" system line: the live card below carries its own
        // running footer, and two indicators for one command is one too many.
        app.begin_shell(cmd);
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        app.shell_cancel = Some(cancel_tx);
        let (cmd, t) = (cmd.to_string(), tx.clone());
        tokio::spawn(async move {
            let outcome = stream::run_local_shell_streaming(cmd.clone(), t.clone(), cancel_rx).await;
            let _ = t
                .send(StreamMsg::ShellDone {
                    cmd,
                    exit: outcome.exit,
                    cancelled: outcome.cancelled,
                })
                .await;
        });
        return;
    }
```

- [ ] In `handle_ctrl_c`, add a new **first** branch, above `if app.streaming`:

```rust
fn handle_ctrl_c(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    // D3: a running `!cmd` is what Ctrl-C ends — it is the thing the user
    // just launched and is watching. Any agent turn keeps running and still
    // has Esc-Esc. Taking the handle means a second press falls through to
    // the behaviour below, unchanged.
    if let Some(cancel) = app.shell_cancel.take() {
        // Err = the command already exited and the receiver is gone; the
        // natural ShellDone is already on its way, so there is nothing to say.
        let _ = cancel.send(());
        return;
    }
    if app.streaming {
        // … unchanged …
```

- [ ] Replace the `ShellDone` arm of `handle_stream` and add a `ShellOutput` arm beside it:

```rust
        StreamMsg::ShellOutput { chunk } => app.append_shell_output(&chunk),
        StreamMsg::ShellDone {
            cmd,
            exit,
            cancelled,
        } => {
            app.shell_cancel = None;
            let output = app.finish_shell(exit, cancelled);
            let block = shell_block(&cmd, &stream::cap_tail(&output, stream::SHELL_MAX_BYTES));
            match route_shell_output(
                cancelled,
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

- [ ] Give `route_shell_output` the new leading parameter and first arm:

```rust
/// Pure so the four routes are testable without pricing or a live agent.
/// The budget gates a NEW turn only, exactly as `submit` does for typed text:
/// a steer rides the turn already being paid for.
fn route_shell_output(
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

- [ ] Update the existing `route_shell_output` call sites in `mod.rs`'s tests: every call gains a leading `false`. Grep `route_shell_output(` to find them.
- [ ] The two existing tests `shell_done_while_idle_starts_a_turn_without_a_user_bubble` and `shell_done_while_streaming_steers_the_live_turn` construct `StreamMsg::ShellDone { cmd, output }`, which no longer exists. Rewrite both to drive the new flow — the assertions are unchanged, only the setup is:

```rust
    /// Idle: the block becomes the outgoing user message, the transcript keeps
    /// the one Shell card and gains no User bubble.
    #[tokio::test]
    async fn shell_done_while_idle_starts_a_turn_without_a_user_bubble() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        app.begin_shell("ls");
        app.append_shell_output("a\nb");
        handle_stream(
            &mut app,
            StreamMsg::ShellDone {
                cmd: "ls".into(),
                exit: Some(0),
                cancelled: false,
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
        app.begin_shell("ls");
        app.append_shell_output("a");
        handle_stream(
            &mut app,
            StreamMsg::ShellDone {
                cmd: "ls".into(),
                exit: Some(0),
                cancelled: false,
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

  Keep whatever assertions follow in the originals; only the `ShellDone`
  literal and the two new setup lines change.

- [ ] Add the new routing and Ctrl-C tests beside them:

```rust
    /// Test 5 — D4: cancelled outranks every other route.
    #[test]
    fn cancelled_shell_output_is_never_sent() {
        for streaming in [true, false] {
            for over_budget in [true, false] {
                let route = route_shell_output(true, streaming, Some("t-1"), over_budget);
                assert!(
                    matches!(route, ShellRoute::Skip(w) if w.contains("cancelled")),
                    "streaming={streaming} over_budget={over_budget}: {route:?}"
                );
            }
        }
        // Not cancelled: the existing routes are untouched.
        assert!(matches!(
            route_shell_output(false, false, None, false),
            ShellRoute::Start
        ));
    }

    /// Test 6 — D3: with a shell running, Ctrl-C takes the shell handle and
    /// leaves the live turn alone.
    #[test]
    fn ctrl_c_ends_the_shell_before_the_turn() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        let task = app.begin_user_turn("working");
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        app.shell_cancel = Some(cancel_tx);

        handle_ctrl_c(&mut app, &tx);

        assert!(cancel_rx.blocking_recv().is_ok(), "the shell was signalled");
        assert!(app.shell_cancel.is_none(), "handle taken");
        assert!(app.streaming, "the turn kept running");
        assert_eq!(app.current_task_id.as_deref(), Some(task.as_str()));

        // A second press, with no shell left, is today's behaviour: it
        // cancels the turn.
        handle_ctrl_c(&mut app, &tx);
        assert!(!app.streaming, "the second press cancelled the turn");
    }
```

  `blocking_recv` inside a `#[test]` (not `#[tokio::test]`) is correct here:
  there is no runtime to block. If `handle_ctrl_c`'s signature forces an
  async context, make the test `#[tokio::test]` and use
  `cancel_rx.await.is_ok()` instead.

- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core -- cli:: > /tmp/t3.log 2>&1; echo $?` → `0`.
- [ ] `cargo fmt --all`; `cargo clippy -p mur-core --all-targets -- -D warnings > /tmp/c3.log 2>&1; echo $?` → `0`.
- [ ] Commit: `feat(murmur): Ctrl-C ends the shell, cancelled output never wakes the agent (#1286 T3)`.

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

- [ ] `grep -rn "SHELL_TIMEOUT_SECS\|push_shell\|run_local_shell\b" mur-core/src mur-agent-runtime/src mur-hub-gui/src-tauri/src` → **no hits** except `run_local_shell_streaming`. Any remaining hit is a caller the tasks missed.
- [ ] `cargo fmt --all -- --check; echo $?` → `0`.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings > /tmp/cw.log 2>&1; echo $?` → `0`.
- [ ] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core > /tmp/tc.log 2>&1; echo $?` → `0`. The full crate is ~5900 tests and takes about a minute after a warm build; a cold build is ~15 minutes.
- [ ] `cargo nextest run -p mur-agent-runtime > /tmp/tr.log 2>&1; echo $?` → `0` (nothing in this plan touches it; this proves it).
- [ ] Hub, **last**: `cd mur-hub-gui/src-tauri && cargo check > /tmp/hub.log 2>&1; echo $?` → `0`. If the worktree lacks `mur-hub-gui/ui/dist`, symlink it from the main checkout first (`ln -s /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui/dist mur-hub-gui/ui/dist`) and **remove the symlink before committing**.
- [ ] Set the spec's Status line to `Implemented in #<PR>` once the PR number exists.
- [ ] Live check, recorded in the PR description:
  - `./install.sh`, then `mur agent restart <one agent>` — restart ONE agent, not `--stale`, so the rest of the machine's fleet stays on the prior build.
  - `!sleep 45` completes instead of dying at 30 s.
  - `!cargo build` (in a repo murmur's cwd can reach) shows output growing line by line with the `running · Ctrl-C to stop` footer.
  - Ctrl-C during that build: the card ends `[cancelled]`, nothing is sent to the agent, and `pgrep -f rustc` shows nothing from that build.
  - With an agent turn streaming, start a `!sleep 30` and press Ctrl-C: the shell ends, the turn keeps going.
- [ ] Open the PR: title `feat(murmur): !cmd streams and is cancelled, never killed by a clock (#1286)`, body = D1–D6 one line each, the §1.4 exit-marker fix called out as a behaviour change, and the live observations.

## Self-review

- **Spec coverage.** D1 → T1 (constant deleted, test 1). D2 → T1 (streaming) + T2 (card) + T4 (footer). D3 → T3 (`handle_ctrl_c` first branch, test 6). D4 → T3 (`route_shell_output` first arm, test 5) + T2 (`[cancelled]` stamp). D5 → T1 (`process_group`, `signal_group`, test 3/4). D6 → T1 (`cap_tail`, `SHELL_CARD_MAX_BYTES`) + T2 (card cap, test 7) + T3 (block cap). §1.4 → T1 (`cap_tail` front marker) + T3 (block uses it). §4 error table: spawn failure → T1; group refusal → T1 (no fallback); non-zero exit → T2; SIGTERM ignored → T1 escalation; double Ctrl-C → T3 (`take`) + test 6; race on send → T3 (`let _ =`); non-UTF-8 → T1 (`pump` carry, test 9). §5 tests 1–10 → T1 (1,2,3,4,8,9), T2 (7,10), T3 (5,6), T4 (footer). §6 out-of-scope items are not implemented anywhere.
- **Cross-task names.** `cap_tail`, `SHELL_MAX_BYTES`, `SHELL_CARD_MAX_BYTES`, `KILL_GRACE`, `ShellOutcome`, `run_local_shell_streaming` (T1) are used verbatim in T2/T3. `begin_shell`, `append_shell_output`, `finish_shell`, `shell_cancel` (T2) are used verbatim in T3. `route_shell_output`'s new arity (T3) is updated at every call site in the same task. `ChatMsg.streaming` (existing) is set in T2 and read in T4.
- **Known soft spots, each with an in-task instruction rather than a guess.** T2's module path to `stream::` from inside the test module (instruction: try both). T3's `blocking_recv` vs `await` in the Ctrl-C test (instruction: switch to `#[tokio::test]` if the signature forces it). T4 had two more — `ChatMsg::new`'s visibility and how a test gets a `&'static Theme` — both resolved before handoff: `ChatMsg::for_test` and `&theme::ANSI` already exist and are used verbatim by the neighbouring `settlement_paint_tests` module, so that task is now fully mechanical.

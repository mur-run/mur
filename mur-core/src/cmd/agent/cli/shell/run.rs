//! Running one local `!command`: spawn it as its own process group, stream
//! both pipes, and stop it on request.
//!
//! The process half of `!cmd`. Its sibling `shell/mod.rs` owns the UI-facing
//! lifecycle (which command is current, where its output goes); nothing here
//! knows about cards or turns.

use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, oneshot};

use super::super::stream::StreamMsg;

/// Cap on the `!command` block forwarded to the agent. Value unchanged from
/// the buffered implementation; what changed is which end survives (§1.4).
pub const SHELL_MAX_BYTES: usize = 8 * 1024;
/// Cap on what one `!command` card keeps in the transcript. Far larger than
/// the agent's block — a human scrolls, a model pays per token — but not
/// unbounded: `!yes` would otherwise grow the TUI without end (D6).
pub const SHELL_CARD_MAX_BYTES: usize = 256 * 1024;
/// SIGTERM -> this -> SIGKILL, on the whole group (D5).
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
    // `killpg(0, …)` does NOT mean "no process" — POSIX defines pgid 0 as
    // THE CALLER'S OWN GROUP, so passing it here would signal murmur and the
    // terminal session it runs in. `spawn` refuses to produce a 0, which is
    // the real defence; this is the second lock on that door, and it also
    // lets a state-machine test use 0 to mean "no real process".
    if pid == 0 {
        return;
    }
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
    if pid == 0 {
        return;
    }
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
pub async fn spawn(cmd: &str) -> std::io::Result<(tokio::process::Child, u32)> {
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
    let mut child = command.spawn()?;
    // `id()` is `None` only once the child has been reaped, which cannot have
    // happened yet. Refuse rather than fall back to 0: that value means "my
    // own process group" to `killpg`, so a silent default would arm every
    // later kill against murmur itself.
    let Some(pid) = child.id() else {
        let _ = child.kill().await;
        return Err(std::io::Error::other(
            "spawned shell reported no pid; refusing to run a command we could not signal",
        ));
    };
    Ok((child, pid))
}

/// Start an argv-shaped command as its own process group.
///
/// Unlike [`spawn`], this never invokes a shell: every argument remains data,
/// which is required for slash commands whose free-text tail came from the
/// user. The returned child otherwise has the same streaming/cancellation
/// contract as a `!command` child and can be passed straight to [`run`].
pub async fn spawn_argv(
    program: &std::path::Path,
    args: &[String],
    env: &[(&str, &str)],
) -> std::io::Result<(tokio::process::Child, u32)> {
    use tokio::process::Command;

    let mut command = Command::new(program);
    command
        .args(args)
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;
    let Some(pid) = child.id() else {
        let _ = child.kill().await;
        return Err(std::io::Error::other(
            "spawned command reported no pid; refusing to run a command we could not signal",
        ));
    };
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
    gen_id: u64,
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

    let mut cancel = cancel;
    let mut cancelled = false;
    let mut end: Option<ShellEnd> = None;
    // `Some` only between SIGTERM and its SIGKILL; dropped when the loop ends.
    let mut escalate: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    loop {
        tokio::select! {
            Some(chunk) = out_rx.recv() => {
                if tx.send(StreamMsg::ShellOutput { gen_id, chunk }).await.is_err() {
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
            if tx
                .send(StreamMsg::ShellOutput { gen_id, chunk })
                .await
                .is_err()
            {
                return;
            }
        }
    })
    .await;
    if drained.is_err() {
        let _ = tx
            .send(StreamMsg::ShellOutput {
                gen_id,
                chunk: DRAIN_INCOMPLETE_NOTE.to_string(),
            })
            .await;
    }

    if cancelled {
        ShellEnd::Cancelled
    } else {
        end.unwrap_or(ShellEnd::Signaled(None))
    }
}

/// Is this pid still alive? Test-only, and shared by both of this module's
/// test suites and its sibling's, so it lives here rather than inside one
/// `mod tests`.
#[cfg(all(test, unix))]
pub(super) fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 probes existence only.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the process-spawning tests use these, and all of them are unix-only.
    #[cfg(unix)]
    use std::time::Duration;
    #[cfg(unix)]
    use tokio::sync::{mpsc, oneshot};

    /// Drive a command to completion, collecting every streamed chunk.
    #[cfg(unix)]
    async fn drive(cmd: &str) -> (String, ShellEnd) {
        let (tx, mut rx) = mpsc::channel(256);
        let (_c_tx, c_rx) = oneshot::channel();
        let (child, pid) = spawn(cmd).await.expect("spawn");
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
        let (child, pid) = spawn("echo one; sleep 0.6; echo two").await.expect("spawn");
        let task = tokio::spawn(run(child, pid, 1, tx, c_rx));
        let first = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("a chunk before the command finished")
            .expect("channel open");
        assert!(
            !task.is_finished(),
            "the command already ended: not streaming"
        );
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
        let (child, pid) = spawn("echo FINAL").await.expect("spawn");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn argv_spawn_does_not_interpret_shell_syntax() {
        let (child, pid) = spawn_argv(
            std::path::Path::new("/bin/echo"),
            &["$(printf injected)".to_string()],
            &[],
        )
        .await
        .expect("spawn argv");
        let (tx, mut rx) = mpsc::channel(16);
        let (_cancel_tx, cancel_rx) = oneshot::channel();
        let end = run(child, pid, 1, tx, cancel_rx).await;
        assert_eq!(end, ShellEnd::Exited(0));
        let mut output = String::new();
        while let Ok(StreamMsg::ShellOutput { chunk, .. }) = rx.try_recv() {
            output.push_str(&chunk);
        }
        assert_eq!(output.trim(), "$(printf injected)");
    }

    /// Tests 3 + 4 — D5: cancel kills the whole group (the grandchild dies,
    /// not just the shell) and returns promptly, not after the 60 s sleep.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_the_group_promptly() {
        let (tx, mut rx) = mpsc::channel(256);
        let (c_tx, c_rx) = oneshot::channel();
        let (child, pid) = spawn("sleep 60 & echo $!; wait").await.expect("spawn");
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
        assert!(
            !alive(grandchild),
            "grandchild {grandchild} outlived the cancel"
        );
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
            assert!(
                out.strip_prefix(TRUNCATED)
                    .unwrap_or(&out)
                    .chars()
                    .all(|c| c == '✓')
            );
        }
    }
}

//! Local `!command` execution for the murmur TUI: spawning, streaming,
//! cancelling, and deciding where a finished command's output goes.
//!
//! Separate from `stream.rs`, which is the A2A streaming bridge and has
//! nothing to do with local processes, and separate from `mod.rs`, which is
//! already far past the repository's 800-line rule (CLAUDE.md §4).

/// What the agent receives for a `!cmd` run. Singular, framed, nothing else:
/// the agent may answer with one line, and the block does not ask for more.
pub(super) fn shell_block(cmd: &str, output: &str) -> String {
    if output.is_empty() {
        format!("[shell command the user ran locally]\n$ {cmd}\n[end of shell output]")
    } else {
        format!("[shell command the user ran locally]\n$ {cmd}\n{output}\n[end of shell output]")
    }
}

/// Where a finished `!cmd` block goes.
#[derive(Debug)]
pub(super) enum ShellRoute {
    /// Idle: start a turn with the block as the user's message.
    Start,
    /// A turn is live: steer it with the block.
    Steer(String),
    /// Nowhere; the note says why. The Shell card still renders.
    Skip(&'static str),
}

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
        return match task_id {
            Some(t) => ShellRoute::Steer(t.to_string()),
            None => {
                ShellRoute::Skip("shell output not sent — a turn is generating without a task id")
            }
        };
    }
    if over_budget {
        return ShellRoute::Skip("↯ shell output not sent — session budget reached");
    }
    ShellRoute::Start
}

mod run;

pub use run::{
    SHELL_CARD_MAX_BYTES, SHELL_MAX_BYTES, SIGKILL_NUM, SIGTERM_NUM, ShellEnd, cap_tail, run,
    signal_group, spawn,
};

use tokio::sync::oneshot;

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
    gen_id: u64,
    slot: Slot,
}

#[derive(Default)]
enum Slot {
    #[default]
    Idle,
    /// Live: Ctrl-C ends it and the spinner ticks for it.
    Running {
        gen_id: u64,
        pid: u32,
        cancel: oneshot::Sender<()>,
    },
    /// Signalled, not yet reaped. The UI has moved on — the card is already
    /// finalised (D11) and this generation retired — but the pid stays so a
    /// quit inside the grace window still has a group to kill.
    Cancelling { gen_id: u64, pid: u32 },
}

impl Slot {
    fn gen_id(&self) -> Option<u64> {
        match self {
            Slot::Idle => None,
            Slot::Running { gen_id, .. } | Slot::Cancelling { gen_id, .. } => Some(*gen_id),
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

    /// May an event from `gen_id` still touch the UI? A teardown retires the
    /// generation, so anything the dying task emits afterwards is dropped
    /// rather than written into a cleared transcript or another channel (D8).
    pub fn accepts(&self, gen_id: u64) -> bool {
        gen_id == self.gen_id
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
        self.gen_id += 1;
        self.slot = Slot::Running {
            gen_id: self.gen_id,
            pid,
            cancel,
        };
        Some(self.gen_id)
    }

    /// The task reported the child is gone — whichever way it went. Called
    /// unconditionally on `ShellDone`, *including* for a retired generation,
    /// because this is the resource question, not the UI one: it is what
    /// clears `Cancelling` so quit stops trying to kill a dead group.
    pub fn done(&mut self, gen_id: u64) {
        if self.slot.gen_id() == Some(gen_id) {
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
    let (gen_id, pid) = match (slot.gen_id(), slot.pid()) {
        (Some(g), Some(p)) => (g, p),
        _ => return false,
    };
    state.gen_id += 1;
    match slot {
        Slot::Running { cancel, .. } if !hard => {
            // Err = the command already exited and the receiver is gone.
            let _ = cancel.send(());
            // The pid stays ours until the task reports back.
            state.slot = Slot::Cancelling { gen_id, pid };
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

#[cfg(test)]
mod tests {
    use super::super::stream::StreamMsg;
    use super::run::alive;
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[test]
    fn shell_output_routes_by_turn_state() {
        assert!(matches!(
            route_shell_output(false, false, None, false),
            ShellRoute::Start
        ));
        assert!(
            matches!(route_shell_output(false, true, Some("t1"), false), ShellRoute::Steer(ref t) if t == "t1")
        );
        assert!(matches!(
            route_shell_output(false, true, None, false),
            ShellRoute::Skip(_)
        ));
        // Budget gates a NEW turn only; a steer rides the turn already paid for.
        assert!(matches!(
            route_shell_output(false, false, None, true),
            ShellRoute::Skip(_)
        ));
        assert!(matches!(
            route_shell_output(false, true, Some("t1"), true),
            ShellRoute::Steer(_)
        ));
    }

    #[test]
    fn shell_block_frames_command_and_output() {
        assert_eq!(
            shell_block("ls", "a\nb"),
            "[shell command the user ran locally]\n$ ls\na\nb\n[end of shell output]"
        );
        assert_eq!(
            shell_block("true", ""),
            "[shell command the user ran locally]\n$ true\n[end of shell output]"
        );
    }

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
        assert!(
            s.accepts(gen1),
            "the refusal must not retire the running command's generation"
        );
        assert!(
            matches!(rx1.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "the first command was neither cancelled nor dropped"
        );
        assert!(s.is_running());
    }

    /// Test 12 — D8: a teardown retires the generation, so late events from
    /// the dying task are no longer accepted.
    ///
    /// `pid: 0` deliberately stands for "no real process": this test is about
    /// the state transitions, and `signal_group` refuses 0 precisely because
    /// `killpg(0, …)` would signal the test runner's own group. A real group
    /// is exercised by `a_quit_after_a_cancel_still_kills_the_group`.
    #[test]
    fn cancel_retires_the_generation() {
        let mut s = ShellState::default();
        let (tx, mut rx) = oneshot::channel();
        let gen_id = s.begin(0, tx).unwrap();
        assert!(s.accepts(gen_id));
        assert!(cancel(&mut s, false), "something was stopped");
        assert!(!s.accepts(gen_id), "stale events are rejected");
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
        let (child, pid) = spawn("trap '' TERM; sleep 60 & echo $!; wait")
            .await
            .expect("spawn");
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
        assert!(
            cancel(&mut state, true),
            "the cancelling slot was still killable"
        );

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
        let gen_id = s.begin(0, tx).unwrap();
        cancel(&mut s, false);
        assert!(!s.accepts(gen_id), "the UI has moved on");
        s.done(gen_id);
        assert!(!s.is_running());
        // Nothing left to kill: a later hard quit is a no-op.
        assert!(!cancel(&mut s, true), "the slot was already empty");
    }

    /// A natural end frees the slot but keeps the generation, because its own
    /// `ShellDone` still has to be accepted.
    #[test]
    fn done_frees_the_slot_without_retiring_the_generation() {
        let mut s = ShellState::default();
        let (tx, _rx) = oneshot::channel();
        let gen_id = s.begin(0, tx).unwrap();
        s.done(gen_id);
        assert!(!s.is_running());
        assert!(s.accepts(gen_id), "its own ShellDone is still wanted");
    }
}

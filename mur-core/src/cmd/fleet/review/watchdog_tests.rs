//! #1758: the mid-turn watchdog. `tripped` is pure (no sockets), so each
//! limit is pinned here; `run_turn_with_retry` is driven through a stub
//! transport whose send never returns on its own, to prove a preemption
//! ends the turn as `Preempted` and is never retried.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use mur_common::limits::Stuck;

use super::super::driver::{RetryOutcome, ReviewTransport, SendGate, run_turn_with_retry};
use super::{MidTurn, TurnWatch};
use crate::a2a_dial::{DialPoll, PollState};
use crate::cmd::fleet::control;

const FLEET: &str = "review-wd";

fn watch(home: &std::path::Path, deadline_left: Duration, stuck: Stuck) -> TurnWatch<'_> {
    TurnWatch {
        mur_home: home,
        fleet_name: FLEET,
        deadline_left,
        stuck,
    }
}

fn state(elapsed: Duration, idle: Duration) -> PollState<'static> {
    PollState {
        elapsed,
        idle,
        task_id: None,
    }
}

const MIN: Duration = Duration::from_secs(60);

#[test]
fn quiet_turn_within_limits_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let w = watch(tmp.path(), 10 * MIN, Stuck::After(5 * MIN));
    assert_eq!(w.tripped(&state(MIN, MIN)), None);
}

#[test]
fn deadline_preempts_a_turn_that_is_still_busy() {
    let tmp = tempfile::tempdir().unwrap();
    let w = watch(tmp.path(), 10 * MIN, Stuck::After(5 * MIN));
    // Still streaming (idle 0), but past what the session had left.
    let s = state(10 * MIN, Duration::ZERO);
    assert_eq!(w.tripped(&s), Some(MidTurn::Deadline));
}

#[test]
fn stuck_counts_idle_time_not_turn_length() {
    let tmp = tempfile::tempdir().unwrap();
    let w = watch(tmp.path(), 60 * MIN, Stuck::After(5 * MIN));
    // A long but active turn is fine; a quiet one is stuck.
    assert_eq!(w.tripped(&state(30 * MIN, MIN)), None);
    assert_eq!(w.tripped(&state(30 * MIN, 5 * MIN)), Some(MidTurn::Stuck));
}

#[test]
fn stuck_off_never_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let w = watch(tmp.path(), 60 * MIN, Stuck::Off);
    assert_eq!(w.tripped(&state(MIN, 50 * MIN)), None);
}

#[test]
fn kill_switch_wins_over_other_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let flag = control::stopped_path(tmp.path(), FLEET);
    std::fs::create_dir_all(flag.parent().unwrap()).unwrap();
    std::fs::write(&flag, "").unwrap();
    let w = watch(tmp.path(), MIN, Stuck::After(MIN));
    let s = state(2 * MIN, 2 * MIN);
    assert_eq!(w.tripped(&s), Some(MidTurn::Stopped));
}

/// A member that never answers: `send_watched` polls the watch (as the
/// real dial does) until it trips, standing in for a live A2A stream.
struct NeverReturns {
    sends: AtomicUsize,
    started: Instant,
}

impl ReviewTransport for NeverReturns {
    fn send(&self, _: &str, _: &serde_json::Value) -> anyhow::Result<String> {
        unreachable!("the loop must use send_watched when a watch is set")
    }

    fn send_watched(
        &self,
        member: &str,
        _: &serde_json::Value,
        w: &TurnWatch<'_>,
    ) -> anyhow::Result<String> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        w.guard(member, |poll: DialPoll<'_>| {
            loop {
                let elapsed = self.started.elapsed();
                // Fake an agent that keeps streaming: never idle.
                let s = state(elapsed, Duration::ZERO);
                if let Some(e) = (poll.check)(&s) {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    }
}

#[test]
fn a_send_that_never_returns_is_preempted_and_not_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let t = NeverReturns {
        sends: AtomicUsize::new(0),
        started: Instant::now(),
    };
    static OPEN: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let g = SendGate {
        boundary: false,
        pre_confirmed: true,
        open: &OPEN,
        watch: Some(watch(tmp.path(), Duration::from_millis(50), Stuck::Off)),
    };
    let out = run_turn_with_retry(
        &t,
        tmp.path(),
        FLEET,
        "main",
        &|_| serde_json::json!({}),
        &mut Vec::new(),
        &mut |_| Ok(()),
        g,
        "unused",
        Duration::ZERO,
    )
    .unwrap();
    assert_eq!(out, RetryOutcome::Preempted(MidTurn::Deadline));
    assert_eq!(
        t.sends.load(Ordering::SeqCst),
        1,
        "a preemption is not a transport fault"
    );
}

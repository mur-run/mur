//! Tests for `turn_cell.rs` (P3b-§6.3, D7): the abort / commit race.

use std::sync::{Arc, Barrier};

use super::turn_cell::{TurnCell, TurnState};

#[test]
fn abort_then_commit_commit_loses() {
    let c = TurnCell::default();
    assert!(c.abort());
    assert!(!c.commit());
    assert_eq!(c.state(), TurnState::Aborted);
}

#[test]
fn commit_then_abort_abort_loses() {
    let c = TurnCell::default();
    assert!(c.commit());
    assert!(!c.abort());
    assert_eq!(c.state(), TurnState::Committed);
}

#[test]
fn second_abort_is_false() {
    let c = TurnCell::default();
    assert!(c.abort());
    assert!(!c.abort());
}

#[test]
fn a_fresh_cell_is_in_flight() {
    assert_eq!(TurnCell::default().state(), TurnState::InFlight);
}

/// AC-P3b-20 / 20a: whichever side wins, exactly one does, and `state()`
/// names the winner.
#[test]
fn concurrent_abort_and_commit_exactly_one_wins() {
    const ROUNDS: usize = 1_000;
    for _ in 0..ROUNDS {
        let cell = Arc::new(TurnCell::default());
        let gate = Arc::new(Barrier::new(2));
        let (c1, g1) = (cell.clone(), gate.clone());
        let (c2, g2) = (cell.clone(), gate.clone());
        let a = std::thread::spawn(move || {
            g1.wait();
            c1.abort()
        });
        let b = std::thread::spawn(move || {
            g2.wait();
            c2.commit()
        });
        let (aborted, committed) = (a.join().unwrap(), b.join().unwrap());
        assert!(aborted ^ committed, "exactly one side must win");
        let want = if aborted {
            TurnState::Aborted
        } else {
            TurnState::Committed
        };
        assert_eq!(cell.state(), want);
    }
}

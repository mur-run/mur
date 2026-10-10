//! The per-turn abort / commit race (P3b-§6.3, D7).
//!
//! The UI's Esc×2 and the transport's "the reply just arrived" both want to
//! decide the turn's fate. Each does one `compare_exchange(InFlight, _)`;
//! whoever wins decides, and the loser learns so from the `false` return.

use std::sync::atomic::{AtomicU8, Ordering};

const IN_FLIGHT: u8 = 0;
const ABORTED: u8 = 1;
const COMMITTED: u8 = 2;

/// Where one member turn stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    InFlight,
    Aborted,
    Committed,
}

/// One cell per turn, shared between the UI thread and the worker.
#[derive(Debug, Default)]
pub struct TurnCell(AtomicU8);

impl TurnCell {
    /// UI side. `true` = the abort won: the reply, if any, is dropped.
    pub fn abort(&self) -> bool {
        self.settle(ABORTED)
    }

    /// Transport side, when `send` returns and before `TurnEnded`.
    /// `true` = the reply is committed and an abort is now too late.
    pub fn commit(&self) -> bool {
        self.settle(COMMITTED)
    }

    pub fn state(&self) -> TurnState {
        match self.0.load(Ordering::Acquire) {
            ABORTED => TurnState::Aborted,
            COMMITTED => TurnState::Committed,
            _ => TurnState::InFlight,
        }
    }

    fn settle(&self, to: u8) -> bool {
        self.0
            .compare_exchange(IN_FLIGHT, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

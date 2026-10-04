//! §8.2: replay-with-damage and the rollback math on Continue. This module
//! sits between `mur-channel`'s `load_events_with_damage` (P4) and the pure
//! `ledger` fold: it decides round boundaries, classifies damage as
//! Partial/Fatal, and computes the monotonic clock/cost adopted on Continue.
//!
//! WORK IN PROGRESS — placeholder so `mod.rs` builds while `ledger`/`schema`
//! land first; the real replay/rollback implementation (AC11a–AC11g) is
//! next.

use super::ledger::Ledger;

/// Outcome of replaying a session's events with damage detection (§8.2).
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayOutcome {
    /// No damage at all — `ledger` is the full, current state.
    Clean(Ledger),
    /// Partial — damage after at least one complete round. `ledger` is
    /// rebuilt up to the last complete round; nothing after is applied.
    Partial {
        ledger: Ledger,
        last_good_round: u32,
        damage_line: Option<usize>,
        damage_reason: String,
    },
    /// Fatal — the first review event is damaged, or no complete round is
    /// valid. Resume is impossible.
    Fatal { damage_reason: String },
}

//! Agent review loop, Phase 1 (spec `docs/superpowers/specs/2026-10-04-agent-review-loop-design.md`).
//!
//! §3–§8: a review session is a two-member ephemeral fleet (`main`,
//! `reviewer`) that alternates turns over the existing A2A transport, with
//! all state folded from signed channel events (§4). This module owns the
//! PURE logic — the finding ledger, verdict/rebuttal validation, stop
//! conditions, and the §8.2 replay/rollback math — kept free of `mur-core`
//! only dependencies per Q5 (§4) so it can be lifted into its own crate if a
//! second crate ever needs to read it.
//!
//! Status: `mur fleet review` (`session.rs`) runs the semi-auto loop over a
//! terminal — main and reviewer turns over A2A (`wire.rs`), a gate before
//! every send, `session_stopped`, fleet removed and channel kept. Malformed
//! verdicts and rebuttals are validated in `verdict.rs` and retried once
//! with a hint (§3.2/§3.4). Not wired yet: §5 auto mode and MURMUR
//! (§6–§7), §3.4 reviewer withdraw/insist on a reject, and the §8.2
//! Continue/Abandon path for a damaged channel (`rollback.rs`). A paused
//! session with a clean channel resumes via `resume.rs`. Items waiting on
//! those carry an `#[allow(dead_code)]` naming the section.

pub mod constants;
pub mod driver;
pub mod ledger;
pub mod loop_driver;
pub mod murmur;
pub mod note;
pub mod prune;
pub mod resume;
// `replay_with_damage` is wired into resume; the Continue-from-checkpoint
// helpers (§8.2 Partial) are not yet, so they stay test-only.
#[allow(dead_code)]
pub mod rollback;
pub mod ruling;
pub mod run_lock;
pub mod schema;
pub mod session;
pub mod settle;
pub mod state;
pub mod turn_cell;
pub mod verdict;
pub mod wire;

#[cfg(test)]
#[path = "driver_tests.rs"]
mod driver_tests;

#[cfg(test)]
#[path = "driver_abort_tests.rs"]
mod driver_abort_tests;

#[cfg(test)]
#[path = "turn_cell_tests.rs"]
mod turn_cell_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "ledger_replay_tests.rs"]
mod ledger_replay_tests;

#[cfg(test)]
#[path = "loop_driver_tests/mod.rs"]
mod loop_driver_tests;

#[cfg(test)]
#[path = "note_tests.rs"]
mod note_tests;

#[cfg(test)]
#[path = "note_murmur_tests.rs"]
mod note_murmur_tests;

#[cfg(test)]
#[path = "no_tampering_tests.rs"]
mod no_tampering_tests;

#[cfg(test)]
#[path = "ruling_tests.rs"]
mod ruling_tests;

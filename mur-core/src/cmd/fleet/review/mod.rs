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
//! (§6–§7), §3.4 reviewer withdraw/insist on a reject, and §8.2 resume
//! (`rollback.rs`). Items waiting on
//! those carry an `#[allow(dead_code)]` naming the section.

pub mod constants;
pub mod driver;
pub mod ledger;
pub mod loop_driver;
pub mod prune;
// §8.2 replay/resume is not wired into `mur fleet review` yet; until it
// is, only its tests call into this module.
#[allow(dead_code)]
pub mod rollback;
pub mod schema;
pub mod session;
pub mod verdict;
pub mod wire;

#[cfg(test)]
#[path = "driver_tests.rs"]
mod driver_tests;

#[cfg(test)]
#[path = "loop_driver_tests/mod.rs"]
mod loop_driver_tests;

#[cfg(test)]
#[path = "no_tampering_tests.rs"]
mod no_tampering_tests;

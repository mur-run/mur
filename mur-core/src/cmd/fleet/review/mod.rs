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
//! WORK IN PROGRESS: the async two-party turn driver (§3.1, A2), the A2A
//! transport wiring, the `mur fleet review` CLI surface, and the MURMUR
//! integration (§5–§7, AC12–AC21) are NOT in this module yet. This file lands
//! only the deterministic core: event schema, ledger fold, verdict/rebuttal
//! state machine, and rollback-on-replay arithmetic. See the coding agent's
//! report for exactly what has run through `cargo build`/`cargo test` and
//! what has not.

pub mod constants;
pub mod ledger;
pub mod rollback;

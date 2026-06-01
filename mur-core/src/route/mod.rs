//! Cost-router orchestrator — difficulty heuristic, routing decisions,
//! and escalation audit ledger.
//!
//! Phase 1 (this module): route decisions + audit ledger.
//! Phase 2 (deferred): governed spawn via `CodingAgentAdapter`.

pub mod heuristic;

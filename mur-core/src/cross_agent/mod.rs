//! Cross-agent observability (M7a), propagation + credit (M7c).
//!
//! Read-only aggregation of peer skill stats, per-agent fitness scoring
//! with half-life decay, cross-agent Jaccard consolidate, pull-side skill
//! propagation, per-agent credit ledger, and skill recombination (M7b).

pub mod consolidate;
pub mod credit;
pub mod fitness;
pub mod intent;
pub mod propagate;
pub mod recombine;
pub mod stats_agg;

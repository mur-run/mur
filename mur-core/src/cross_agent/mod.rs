//! Cross-agent observability (M7a).
//!
//! Read-only aggregation of peer skill stats, per-agent fitness scoring
//! with half-life decay, and cross-agent Jaccard consolidate.

pub mod consolidate;
pub mod fitness;
pub mod recombine;
pub mod stats_agg;

//! Coordination protocol types for multi-step agent workflows.
//!
//! This module defines the shared vocabulary for Plans, Steps, Microsteps,
//! SDLC phases, determinism modes, failure categories, and recovery actions.
//! It is the **single source of truth** for both mur-commander and mur-runtime.
//!
//! # Conformance
//!
//! Hosts implement [`ConformanceAdapter`] and pass [`PlanLoadingSuite`] to
//! prove they parse and validate plans correctly. See the
//! `tests/coordination_conformance.rs` integration test for the 10-test suite.

pub mod types;
pub mod plan;
pub mod conformance;

pub use types::{
    ConformanceLevel, DeterminismMode, FailureCategory, Phase, RecoveryAction,
};
pub use plan::{Plan, Step};
pub use conformance::{ConformanceAdapter, PlanLoadingSuite};

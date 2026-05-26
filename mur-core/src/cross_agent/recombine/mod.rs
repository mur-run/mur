//! M7b — Skill recombination engine.
//!
//! Two parent skills produce a third under one of three strategies:
//! Union (superset merge), Intersection (overlap merge), LLM (delegated).
//! Output strictly on the invoking agent — peer state is never written.

pub mod llm;
pub mod peer_ref;
pub mod strategy;

pub use strategy::{FitnessCtx, RecombineStrategy};

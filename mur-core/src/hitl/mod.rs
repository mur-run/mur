//! Risk-tiered, hash-pinned HITL gate for the channel executor (v3c).
pub mod gate;
/// The pin lives in `mur-common` since P3 so the agent runtime can share the
/// canonicalisation without depending on this crate; re-exported so every
/// `crate::hitl::pin::action_hash` call site stays valid.
pub use mur_common::hitl::pin;

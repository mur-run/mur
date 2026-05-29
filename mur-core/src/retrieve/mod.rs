pub mod gate;
pub mod scoring;
pub mod skill_candidates;

// ---------- P1.3: sources retrieve (gated behind "sources" feature) ----------

#[cfg(feature = "sources")]
mod unified;

#[cfg(feature = "sources")]
#[allow(unused_imports)] // HitKind wired to CLI formatter in P1.4
pub use unified::{HitKind, UnifiedHit, retrieve_unified};

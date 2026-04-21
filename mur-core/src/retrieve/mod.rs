pub mod gate;
pub mod scoring;

// ---------- P1.3: sources retrieve (gated behind "sources" feature) ----------

#[cfg(feature = "sources")]
mod unified;

#[cfg(feature = "sources")]
pub use unified::{HitKind, UnifiedHit, retrieve_unified};

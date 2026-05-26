//! Pull-side propagation sweep (M7c).
//!
//! Each invocation scans peers, filters by fitness gates, and pulls
//! eligible skills via the existing M4b `agent://` install path.

pub mod candidates;
pub mod install_ctx;

pub use install_ctx::InstallContext;

//! Shared GUI core library for mur Hub and mur-agent-gui.
//!
//! M-h0: empty skeleton. Later milestones populate:
//!   - `sidecar` — spawn/supervise mur-agent-runtime children
//!   - `companion_bridge` — filesystem watcher on companion/inbox/
//!   - `a2a` — A2A v0.3 unix-socket client
//!
//! See `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md` §3.1.

pub mod autostart;
pub mod companion_bridge;
pub mod discovery;
pub mod event_bus;
pub mod expression;
pub mod image_gen;
pub mod ipc;
pub mod oauth_bridge;
pub mod panel_bridge;
pub mod render;
pub mod revocations;
pub mod sidecar;
pub mod stub;
pub mod voice;

pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_version_is_non_empty() {
        assert!(!CRATE_VERSION.is_empty());
    }
}

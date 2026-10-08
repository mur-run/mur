//! Agent-side half of the git push broker (spec §2): `git_push_request`,
//! `git_push_status`, `git_push_cancel`.
//!
//! The agent never pushes. `git_push_request` builds a pack in the agent's own
//! sandboxed repo, signs a request file and drops both in the agent's inbox; the
//! daemon verifies, imports into a broker-private repo, asks a human, and pushes.
//!
//! Registry location (plan decision A): this side reads
//! `<mur_home>/git-push/registry.yaml` itself to resolve `repo_id`. That is safe
//! only because the agent cannot write it — `LaunchChain` write-protects the
//! whole broker dir (premise 1) — and because no tool output ever echoes the
//! mapping back (premise 2).

#[cfg(unix)]
mod registry;
#[cfg(unix)]
mod request;
#[cfg(unix)]
mod status;

#[cfg(unix)]
pub use registry::{GitPushRegistry, RegistryEntry};
#[cfg(unix)]
pub use request::{GitPushCtx, GitPushRequestTool};
#[cfg(unix)]
pub use status::{GitPushCancelTool, GitPushStatusTool};

pub const GIT_PUSH_REQUEST: &str = "git_push_request";
pub const GIT_PUSH_STATUS: &str = "git_push_status";
pub const GIT_PUSH_CANCEL: &str = "git_push_cancel";

/// `git_push.enabled` in the global config (default off). Read at tool
/// registration and by the sandbox builder; never from the agent profile.
pub fn enabled(mur_home: &std::path::Path) -> bool {
    mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"))
        .git_push
        .enabled
}

/// Every rejection the model sees starts with a stable wire code.
#[cfg(unix)]
fn invalid(detail: &str) -> super::ToolError {
    super::ToolError::InvalidInput(format!("invalid_request: {detail}"))
}

#[cfg(all(test, unix))]
mod tests;

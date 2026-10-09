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

/// Is `agent` allowed the push tools per the global config? Deny-by-default:
/// the broker must be `enabled` AND the agent named in `agents` (exact,
/// canonical name — the same match `fleet_run.agents` uses).
pub fn allowed(cfg: &mur_common::config::GitPushConfig, agent: &str) -> bool {
    cfg.enabled && cfg.agents.iter().any(|a| a == agent)
}

/// [`allowed`] against `<mur_home>/config.yaml`. Read at tool registration and
/// by the sandbox builder; never from the agent profile.
pub fn agent_enabled(mur_home: &std::path::Path, agent: &str) -> bool {
    allowed(
        &mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).git_push,
        agent,
    )
}

/// Every rejection the model sees starts with a stable wire code.
#[cfg(unix)]
fn invalid(detail: &str) -> super::ToolError {
    super::ToolError::InvalidInput(format!("invalid_request: {detail}"))
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(test)]
mod allow_tests {
    use super::allowed;
    use mur_common::config::GitPushConfig;

    #[test]
    fn allowed_is_deny_by_default_on_both_axes() {
        assert!(!allowed(&GitPushConfig::default(), "mur"));
        let listed_but_off = GitPushConfig {
            enabled: false,
            agents: vec!["mur".into()],
        };
        assert!(!allowed(&listed_but_off, "mur"));
        let on_but_unlisted = GitPushConfig {
            enabled: true,
            agents: vec![],
        };
        assert!(!allowed(&on_but_unlisted, "mur"));
        let on = GitPushConfig {
            enabled: true,
            agents: vec!["mur".into()],
        };
        assert!(allowed(&on, "mur"));
        assert!(
            !allowed(&on, "dr_worker_1"),
            "a research worker is not named"
        );
        assert!(
            !allowed(&on, "Mur"),
            "exact canonical match, like fleet_run"
        );
    }
}

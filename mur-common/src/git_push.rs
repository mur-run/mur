//! On-disk layout shared by the git push broker (daemon side) and its agent-side
//! tools (runtime side). Lives here because both `mur-agent-runtime` and
//! `mur-daemon` read it and neither may depend on the other.
//!
//! Two trust zones, kept as separate paths on purpose:
//!
//! - `<mur_home>/git-push/` is **broker-owned**. The daemon (or `mur` setup)
//!   writes it; no agent sandbox may write it. `LaunchChain` puts the whole
//!   directory in its write-protected set, so on Linux any write grant that
//!   overlaps it is dropped whole, on macOS SBPL denies it, and the file tools
//!   refuse it. The registry is the reason: an agent that could write it could
//!   remap a `repo_id` to any path and override the human's enrollment.
//! - `<agent_home>/inbox/git-push*` is **agent-owned**: the agent drops signed
//!   requests and cancel markers there; the daemon verifies before acting.
//!
//! The agent may READ exactly two broker paths — [`registry_path`] and its own
//! [`status_dir`] — never the whole directory, which also holds broker-private
//! state (pending table, per-request repos).

use std::path::{Path, PathBuf};

/// Broker-owned directory under `<mur_home>`.
pub const GIT_PUSH_DIR: &str = "git-push";
/// `repo_id` → sandbox repo path + enrolled remote ids. Daemon writes, agent reads.
pub const REGISTRY_FILE: &str = "registry.yaml";
/// Per-agent status files the daemon publishes: `status/<agent>/<request_id>.yaml`.
pub const STATUS_DIR: &str = "status";
/// Broker-private state (pending table, per-request repos). No agent grant may reach it.
pub const PRIVATE_DIR: &str = "private";
/// Agent-side request drop, relative to the agent home.
pub const INBOX_DIR: &str = "inbox/git-push";
/// Agent-side cancel markers, relative to the agent home.
pub const CANCEL_DIR: &str = "inbox/git-push-cancel";

/// `<mur_home>/git-push`.
pub fn broker_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(GIT_PUSH_DIR)
}

/// `<mur_home>/git-push/registry.yaml`.
pub fn registry_path(mur_home: &Path) -> PathBuf {
    broker_dir(mur_home).join(REGISTRY_FILE)
}

/// `<mur_home>/git-push/status/<agent>`.
pub fn status_dir(mur_home: &Path, agent: &str) -> PathBuf {
    broker_dir(mur_home).join(STATUS_DIR).join(agent)
}

/// `<mur_home>/git-push/private`.
pub fn private_dir(mur_home: &Path) -> PathBuf {
    broker_dir(mur_home).join(PRIVATE_DIR)
}

/// `<agent_home>/inbox/git-push`.
pub fn inbox_dir(agent_home: &Path) -> PathBuf {
    agent_home.join(INBOX_DIR)
}

/// `<agent_home>/inbox/git-push-cancel`.
pub fn cancel_dir(agent_home: &Path) -> PathBuf {
    agent_home.join(CANCEL_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_side_paths_stay_outside_the_broker_dir() {
        let mur = Path::new("/m");
        let agent = mur.join("agents").join("bob");
        let broker = broker_dir(mur);
        assert!(!inbox_dir(&agent).starts_with(&broker));
        assert!(!cancel_dir(&agent).starts_with(&broker));
        assert!(registry_path(mur).starts_with(&broker));
        assert!(status_dir(mur, "bob").starts_with(&broker));
        assert!(private_dir(mur).starts_with(&broker));
        assert!(
            !status_dir(mur, "bob").starts_with(private_dir(mur)),
            "the agent-readable status dir must not sit inside broker-private state"
        );
    }
}

//! Typed error surface for the `agent_admin` library API.
//!
//! Per project conventions (CLAUDE.md / hints): use `thiserror` at
//! public API boundaries, `anyhow` for application-level errors. The
//! CLI dispatchers in `cmd::agent::*` and the Tauri command handlers
//! in `mur-agent-gui` are application-level and convert
//! `AgentAdminError` to their preferred shape (anyhow::Error or
//! String).
//!
//! Variants are intentionally specific where the failure mode is
//! known to be common; less-specific failures funnel through
//! `Backend` until the dependency inversion (#5 from the PR #41
//! review) moves more logic into `agent_admin/` and lets us name
//! more cases.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentAdminError {
    /// `~/.mur/agents/<name>/` doesn't exist.
    #[error("agent '{name}' not found at {path}")]
    AgentNotFound { name: String, path: PathBuf },

    /// `profile.yaml` exists but couldn't be parsed.
    #[error("malformed profile.yaml at {path}: {source}")]
    ProfileMalformed {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },

    /// I/O failure reading or writing under `~/.mur/`.
    #[error("filesystem I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A skill / MCP / etc. lookup found no matching entry.
    #[error("{kind} '{query}' not found in agent '{agent}'")]
    NotFound {
        agent: String,
        kind: &'static str,
        query: String,
    },

    /// Catch-all for failure modes that haven't been promoted to
    /// specific variants yet — typically an `anyhow::Error` bubbling
    /// up from the `cmd::agent::cmd_*` delegators. Will shrink as
    /// the dependency inversion (#5) progresses.
    #[error(transparent)]
    Backend(#[from] anyhow::Error),
}

/// Convenient `Result` alias used throughout `agent_admin`.
pub type AgentAdminResult<T> = std::result::Result<T, AgentAdminError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn agent_not_found_displays_path_and_name() {
        let err = AgentAdminError::AgentNotFound {
            name: "demo".into(),
            path: PathBuf::from("/tmp/.mur/agents/demo"),
        };
        let msg = err.to_string();
        assert!(msg.contains("demo"), "missing name: {msg}");
        assert!(msg.contains("/tmp/.mur/agents/demo"), "missing path: {msg}");
    }

    #[test]
    fn not_found_carries_kind_so_callers_can_match() {
        let err = AgentAdminError::NotFound {
            agent: "demo".into(),
            kind: "skill",
            query: "web-search".into(),
        };
        match err {
            AgentAdminError::NotFound { kind, .. } => assert_eq!(kind, "skill"),
            _ => panic!("expected NotFound variant"),
        }
    }

    #[test]
    fn anyhow_converts_via_from_for_backend_compat() {
        let any: anyhow::Error = anyhow::anyhow!("oops");
        let wrapped: AgentAdminError = any.into();
        assert!(matches!(wrapped, AgentAdminError::Backend(_)));
    }
}

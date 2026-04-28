//! HTTP routes for `~/.mur/agents/*` — read-only Phase 4.
//!
//! Mounted under `/api/v1/agents` by [`crate::server::build_router`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use serde::Serialize;

use crate::server::{AppError, AppState};

pub mod detail;
pub mod evals;
pub mod list;
pub mod telemetry;

/// Build the nested `/api/v1/agents` router (Phase 4 read-only routes).
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", axum::routing::get(list::handler))
        .route("/{name}", axum::routing::get(detail::handler))
        .route(
            "/{name}/telemetry",
            axum::routing::get(telemetry::tail_handler),
        )
        .route(
            "/{name}/stream",
            axum::routing::get(telemetry::stream_handler),
        )
        .route("/{name}/evals", axum::routing::get(evals::list_handler))
        .route(
            "/{name}/evals/{run_id}",
            axum::routing::get(evals::detail_handler),
        )
        // Explicit 404 fallback: without this, unknown /api/v1/agents/* paths
        // bubble up to the outer router's SPA fallback and return 200 HTML.
        .fallback(|| async { StatusCode::NOT_FOUND })
}

// ─── Shared helpers ────────────────────────────────────────────────

/// Validate an agent name from a URL path parameter. Agent names follow a
/// strict allowlist to prevent directory traversal via `agents_dir.join(name)`.
pub(crate) fn validate_agent_name(name: &str) -> Result<(), AppError> {
    let safe = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        && !name.contains("..");
    if !safe {
        return Err(AppError::BadRequest(format!("invalid agent name '{name}'")));
    }
    Ok(())
}

/// Absolute path to `~/.mur/agents/<name>/`. Does not check existence.
pub(crate) fn agent_home(agents_dir: &Path, name: &str) -> PathBuf {
    agents_dir.join(name)
}

/// Three-state classification of an agent's runtime state.
///
/// Mirrors `mur-core/src/cmd/agent.rs::classify` so the HTTP API and CLI
/// agree on what "running" means.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatusKind {
    /// Lock present and the recorded pid is alive.
    Running,
    /// Lock present but the pid is not alive (crash/kill — orphan lock).
    Stale,
    /// No lock file.
    Stopped,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AgentStatusInfo {
    pub kind: AgentStatusKind,
    pub pid: Option<u32>,
}

/// Inspect `<home>/running.lock` and classify the agent's state.
///
/// Mirrors `mur-core/src/cmd/agent.rs::classify` so the HTTP and CLI surfaces
/// agree on what "running" means.
pub(crate) fn agent_status(home: &Path) -> AgentStatusInfo {
    let lock_path = home.join("running.lock");
    let bytes = match std::fs::read(&lock_path) {
        Ok(b) => b,
        Err(_) => {
            return AgentStatusInfo {
                kind: AgentStatusKind::Stopped,
                pid: None,
            };
        }
    };
    // Lock is JSON: see mur-agent-runtime/src/lock_file.rs.
    let lock: mur_common::LockFile = match serde_json::from_slice(&bytes) {
        Ok(l) => l,
        Err(_) => {
            // Malformed lock — treat as stale rather than running so dashboards
            // don't paint dead agents green.
            return AgentStatusInfo {
                kind: AgentStatusKind::Stale,
                pid: None,
            };
        }
    };
    let kind = if pid_alive(lock.pid) {
        AgentStatusKind::Running
    } else {
        AgentStatusKind::Stale
    };
    AgentStatusInfo {
        kind,
        pid: Some(lock.pid),
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // Signal 0 is the standard liveness probe — returns 0 if the process exists
    // and we have permission to signal it, ESRCH if not.
    // SAFETY: kill(2) is safe to call with signal 0; no signal is delivered.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    // Windows agents are not supported in P0a; treat any present lock as live.
    true
}

//! HTTP routes for `~/.mur/agents/*` — read-only Phase 4.
//!
//! Mounted under `/api/v1/agents` by [`crate::server::build_router`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;

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

/// True when `<agent_home>/running.lock` exists. Per the runtime spec
/// the lock is created on supervisor start and removed on clean exit.
pub(crate) fn is_running(home: &Path) -> bool {
    home.join("running.lock").exists()
}

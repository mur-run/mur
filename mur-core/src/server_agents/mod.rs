//! HTTP routes for `~/.mur/agents/*` — read-only Phase 4.
//!
//! Mounted under `/api/v1/agents` by [`crate::server::build_router`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::Router;

use crate::server::AppState;

pub mod list;
pub mod detail;
pub mod telemetry;
pub mod evals;

/// Build the nested `/api/v1/agents` router. Returns an empty router for
/// now — handlers are wired in by later tasks.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", axum::routing::get(list::handler))
        .route("/{name}", axum::routing::get(detail::handler))
        // Explicit 404 fallback: without this, unknown /api/v1/agents/* paths
        // bubble up to the outer router's SPA fallback and return 200 HTML.
        .fallback(|| async { StatusCode::NOT_FOUND })
}

// ─── Shared helpers ────────────────────────────────────────────────

/// Absolute path to `~/.mur/agents/<name>/`. Does not check existence.
pub(crate) fn agent_home(agents_dir: &Path, name: &str) -> PathBuf {
    agents_dir.join(name)
}

/// True when `<agent_home>/running.lock` exists. Per the runtime spec
/// the lock is created on supervisor start and removed on clean exit.
pub(crate) fn is_running(home: &Path) -> bool {
    home.join("running.lock").exists()
}

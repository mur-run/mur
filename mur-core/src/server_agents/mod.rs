//! HTTP routes for `~/.mur/agents/*` — read-only Phase 4.
//!
//! Mounted under `/api/v1/agents` by [`crate::server::build_router`].
//!
//! # Response envelope
//!
//! These handlers intentionally return `Json<T>` directly, not the
//! `{data, meta: {source, version, pattern_count}}` envelope that
//! [`crate::server::wrap`] adds to patterns / workflows responses.
//! The `pattern_count` field is patterns-specific and would be meaningless
//! for agent data; rather than emit a misleading `pattern_count: 0`, agent
//! routes skip the envelope entirely.
//!
//! Future contributors: do **not** wrap these responses to "match" the
//! patterns / workflows shape — the difference is intentional. If a fully
//! consistent envelope is wanted across the API, generalize `wrap()` first
//! (see issue #37 for the design discussion).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::server::{AppError, AppState};
use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::response::IntoResponse;

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
        .layer(axum::middleware::from_fn(loopback_only))
}

/// Reject any request whose peer is not loopback. This is a temporary stand-in
/// for Phase 6 auth — until real auth lands, the agents API is treated as a
/// localhost-only debugging surface.
///
/// In tests using `tower::ServiceExt::oneshot` no `ConnectInfo` is attached,
/// so the extractor returns `None` and we allow the request. Production boot
/// must use `into_make_service_with_connect_info::<SocketAddr>()` so the
/// extension IS present and non-loopback callers actually get 403.
pub(crate) async fn loopback_only(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Extract ConnectInfo from request extensions directly rather than using the
    // FromRequestParts extractor parameter form. The workspace carries both axum
    // 0.7 (via qdrant-client → tonic 0.12) and axum 0.8 (direct dep); the
    // extractor signature fails to compile because trait bounds resolve against
    // the wrong version. See CONTRIBUTING.md § "axum middleware in this
    // workspace" and issue #39 for the full explanation and the intended fix.
    let is_loopback = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip().is_loopback())
        .unwrap_or(true); // no ConnectInfo (e.g. oneshot tests) → allow

    if !is_loopback {
        return (
            StatusCode::FORBIDDEN,
            "agents API is loopback-only until auth lands (issue #38)",
        )
            .into_response();
    }
    next.run(req).await
}

// ─── Shared helpers ────────────────────────────────────────────────

/// Validate an agent name from a URL path parameter. Delegates to the
/// canonical validator in `mur_common` so the HTTP API and CLI share a single
/// source of truth.
pub(crate) fn validate_agent_name(name: &str) -> Result<(), AppError> {
    mur_common::validate_agent_name(name).map_err(|e| AppError::BadRequest(e.to_string()))
}

/// Absolute path to `~/.mur/agents/<name>/`. Does not check existence.
pub(crate) fn agent_home(agents_dir: &Path, name: &str) -> PathBuf {
    agents_dir.join(name)
}

// Re-export the canonical status types from mur_common so callers in
// detail.rs and list.rs can reference them as `super::AgentStatusKind`.
// This is the single source of truth for lock parsing + 3-state classification
// (closes #36). Previously this logic was duplicated here, in cmd/agent.rs,
// and in mur-agent-runtime/src/lock_file.rs.
pub(crate) use mur_common::lock_file::AgentStatusKind;
// AgentStatusInfo is the name local callers use; maps to the common AgentStatus.
pub(crate) use mur_common::lock_file::AgentStatus as AgentStatusInfo;

/// Inspect `<home>/running.lock` and classify the agent's state.
///
/// Delegates to `mur_common::lock_file::classify` — single source of truth
/// for read + pid_alive + 3-state classification (closes #36).
pub(crate) fn agent_status(home: &Path) -> AgentStatusInfo {
    mur_common::lock_file::classify(&home.join("running.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn build_state(tmp: &tempfile::TempDir) -> crate::server::AppState {
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        // Also create the other dirs build_router might need
        std::fs::create_dir_all(tmp.path().join("patterns")).unwrap();
        std::fs::create_dir_all(tmp.path().join("workflows")).unwrap();
        std::fs::create_dir_all(tmp.path().join("pipelines")).unwrap();
        crate::server::AppState {
            patterns_dir: tmp.path().join("patterns"),
            workflows_dir: tmp.path().join("workflows"),
            pipelines_dir: tmp.path().join("pipelines"),
            agents_dir,
            index_dir: tmp.path().join("index"),
            config: crate::server::ServerConfig { readonly: false },
            events_tx: tokio::sync::broadcast::channel(64).0,
        }
    }

    #[tokio::test]
    async fn loopback_only_allows_127_0_0_1() {
        let tmp = tempfile::tempdir().unwrap();
        let app = crate::server::build_router(build_state(&tmp));
        let mut req = Request::builder()
            .uri("/api/v1/agents")
            .body(Body::empty())
            .unwrap();
        // Inject ConnectInfo as if a real client connected from loopback.
        req.extensions_mut()
            .insert(ConnectInfo::<SocketAddr>("127.0.0.1:5555".parse().unwrap()));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn loopback_only_rejects_lan_ip() {
        let tmp = tempfile::tempdir().unwrap();
        let app = crate::server::build_router(build_state(&tmp));
        let mut req = Request::builder()
            .uri("/api/v1/agents")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo::<SocketAddr>(
            "192.168.1.42:5555".parse().unwrap(),
        ));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            body.contains("loopback-only"),
            "body should explain why: {body}"
        );
    }

    #[tokio::test]
    async fn loopback_only_allows_ipv6_loopback() {
        let tmp = tempfile::tempdir().unwrap();
        let app = crate::server::build_router(build_state(&tmp));
        let mut req = Request::builder()
            .uri("/api/v1/agents")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo::<SocketAddr>("[::1]:5555".parse().unwrap()));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

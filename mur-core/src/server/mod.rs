//! Local API server for MUR — `mur serve`.
//!
//! Exposes the pattern and workflow stores over HTTP so the web dashboard
//! (mur.run SPA or localhost dev) can read and write data.
//!
//! Handlers are split per resource into sibling modules; this file owns
//! the shared types (AppState, AppError, ApiResponse), the small "system"
//! handlers (ws / ui / health / rates), and the router assembly.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Json, Path, Request, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use rust_embed::Embed;
use serde::Serialize;
use tokio::sync::broadcast;
use tower_http::cors::{AllowHeaders, AllowMethods, CorsLayer};

use crate::store::pipeline_yaml::PipelineYamlStore;
use crate::store::spot_rate::{fetch_usd_rate, fetch_usd_rates};
use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::YamlStore;

mod context;
mod governance;
mod mobile;
mod patterns;
mod pipelines;
mod schedules;
mod search;
mod sessions;
mod signals;
mod skills;
mod stats;
mod workflows;

#[cfg(test)]
mod tests;

use context::{context_feedback, context_ingest, context_retrieve};
use patterns::{create_pattern, delete_pattern, get_pattern, list_patterns, update_pattern};
use pipelines::{
    create_pipeline, delete_pipeline, get_pipeline, list_pipelines, run_pipeline,
    run_pipeline_expr, update_pipeline, validate_pipeline,
};
use search::search_patterns;
use sessions::{
    bulk_delete_sessions, delete_session, get_session, get_session_events, list_sessions,
    patch_session,
};
use signals::batch_signals;
use skills::{create_skill, delete_skill, get_skill, list_skills, update_skill};
use stats::{get_links, get_stats, get_tags};
use workflows::{
    create_workflow, delete_workflow, extract_workflow_from_session, get_workflow, list_workflows,
    search_workflows, update_workflow,
};

// Web UI assets — set MUR_WEB_DIST env at build time for full dashboard,
// falls back to a placeholder page if not set.
#[derive(Embed)]
#[folder = "$MUR_WEB_DIST"]
#[prefix = ""]
#[include = "*.html"]
#[include = "*.js"]
#[include = "*.css"]
#[include = "*.svg"]
#[include = "*.png"]
#[include = "*.ico"]
#[include = "*.woff2"]
#[include = "*.json"]
struct WebAssets;

// ─── Shared application state ──────────────────────────────────────

/// Server configuration flags.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub readonly: bool,
}

/// Shared state passed to all handlers.
#[derive(Clone)]
pub struct AppState {
    pub patterns_dir: PathBuf,
    pub workflows_dir: PathBuf,
    pub pipelines_dir: PathBuf,
    /// `~/.mur/agents/` — root for per-agent profiles, telemetry, evals.
    pub agents_dir: PathBuf,
    /// Path to the LanceDB vector index (`~/.mur/index`).
    /// When present the context endpoint uses hybrid scoring.
    pub index_dir: PathBuf,
    pub config: ServerConfig,
    pub events_tx: broadcast::Sender<String>,
}

impl AppState {
    pub(super) fn pattern_store(&self) -> Result<YamlStore, AppError> {
        YamlStore::new(self.patterns_dir.clone()).map_err(AppError::Internal)
    }

    pub(super) fn workflow_store(&self) -> Result<WorkflowYamlStore, AppError> {
        WorkflowYamlStore::new(self.workflows_dir.clone()).map_err(AppError::Internal)
    }

    pub(super) fn pipeline_store(&self) -> Result<PipelineYamlStore, AppError> {
        PipelineYamlStore::new(self.pipelines_dir.clone()).map_err(AppError::Internal)
    }

    /// `~/.mur/skills/` derived from `patterns_dir` sibling.
    pub(super) fn skills_dir(&self) -> std::path::PathBuf {
        self.patterns_dir
            .parent()
            .unwrap_or(&self.patterns_dir)
            .join("skills")
    }

    /// `~/.mur` — parent of `patterns_dir`.
    pub(super) fn mur_home(&self) -> std::path::PathBuf {
        self.patterns_dir
            .parent()
            .unwrap_or(&self.patterns_dir)
            .to_path_buf()
    }
}

// ─── Error type ────────────────────────────────────────────────────

#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    Readonly,
    BadRequest(String),
    Forbidden(String),
    Internal(anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::Readonly => (
                StatusCode::FORBIDDEN,
                "Server is in read-only mode".to_string(),
            ),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg),
            AppError::Internal(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        let body = serde_json::json!({ "error": message });
        (status, Json(body)).into_response()
    }
}

// ─── Envelope ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct ApiResponse<T: Serialize> {
    data: T,
    meta: ApiMeta,
}

#[derive(Serialize)]
struct ApiMeta {
    pattern_count: usize,
    server_version: &'static str,
}

pub(super) fn wrap<T: Serialize>(data: T, pattern_count: usize) -> Json<ApiResponse<T>> {
    Json(ApiResponse {
        data,
        meta: ApiMeta {
            pattern_count,
            server_version: env!("CARGO_PKG_VERSION"),
        },
    })
}

// ─── Router ────────────────────────────────────────────────────────

/// Build the axum router with all API endpoints. No authentication — this is the
/// default for the loopback bind. Use [`build_router_with_auth`] to require a
/// bearer token (done automatically when the server is bound beyond loopback).
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_router(state: AppState) -> Router {
    build_router_with_auth(state, None)
}

/// Build the router, optionally requiring a bearer token on `/api/*` routes.
/// `/api/v1/health` and the static web UI stay reachable without a token.
pub fn build_router_with_auth(state: AppState, auth_token: Option<Arc<str>>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:5173".parse().unwrap(),
            "http://localhost:3847".parse().unwrap(),
            "https://mur.run".parse().unwrap(),
            "https://www.mur.run".parse().unwrap(),
            "https://dashboard.mur.run".parse().unwrap(),
        ])
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any());

    let router = Router::new()
        // Health
        .route("/api/v1/health", get(health))
        // Patterns CRUD
        .route("/api/v1/patterns", get(list_patterns))
        .route("/api/v1/patterns", post(create_pattern))
        .route("/api/v1/patterns/{id}", get(get_pattern))
        .route("/api/v1/patterns/{id}", put(update_pattern))
        .route("/api/v1/patterns/{id}", delete(delete_pattern))
        // Skills CRUD
        .route("/api/v1/skills", get(list_skills))
        .route("/api/v1/skills", post(create_skill))
        .route("/api/v1/skills/{name}", get(get_skill))
        .route("/api/v1/skills/{name}", put(update_skill))
        .route("/api/v1/skills/{name}", delete(delete_skill))
        // Workflows CRUD
        .route("/api/v1/workflows", get(list_workflows))
        .route("/api/v1/workflows", post(create_workflow))
        .route("/api/v1/workflows/{id}", get(get_workflow))
        .route("/api/v1/workflows/{id}", put(update_workflow))
        .route("/api/v1/workflows/{id}", delete(delete_workflow))
        // Stats & metadata
        // Schedules (read-only; the CLI owns writing)
        .route("/api/v1/schedules", get(schedules::list_schedules))
        .route("/api/v1/stats", get(get_stats))
        .route("/api/v1/tags", get(get_tags))
        .route("/api/v1/links/{id}", get(get_links))
        // Search
        .route("/api/v1/search", post(search_patterns))
        .route("/api/v1/workflows/search", post(search_workflows))
        // Context API (retrieve, ingest, feedback)
        .route("/api/v1/context", post(context_retrieve))
        .route("/api/v1/ingest", post(context_ingest))
        .route("/api/v1/feedback", post(context_feedback))
        // Cloud-mode signal ingestion
        .route("/api/v1/core/signals/batch", post(batch_signals))
        // Sessions
        .route("/api/v1/sessions", get(list_sessions))
        .route(
            "/api/v1/sessions/{id}",
            get(get_session).delete(delete_session).patch(patch_session),
        )
        .route("/api/v1/sessions/{id}/events", get(get_session_events))
        .route("/api/v1/sessions/bulk-delete", post(bulk_delete_sessions))
        // Extract workflow draft from session
        .route(
            "/api/v1/workflows/extract-from-session/{session_id}",
            post(extract_workflow_from_session),
        )
        // Pipelines CRUD + run + validate
        .route("/api/v1/pipelines", get(list_pipelines))
        .route("/api/v1/pipelines", post(create_pipeline))
        .route("/api/v1/pipelines/validate", post(validate_pipeline))
        .route("/api/v1/pipelines/run", post(run_pipeline_expr))
        .route("/api/v1/pipelines/{id}", get(get_pipeline))
        .route("/api/v1/pipelines/{id}", put(update_pipeline))
        .route("/api/v1/pipelines/{id}", delete(delete_pipeline))
        .route("/api/v1/pipelines/{id}/run", post(run_pipeline))
        // Exchange rates (Frankfurter API proxy)
        .route("/api/v1/rates", get(get_rates))
        .route("/api/v1/rates/{currency}", get(get_rate))
        .route("/api/v1/mobile/pair-uri", get(mobile::get_pair_uri))
        .route("/api/v1/mobile/devices", get(mobile::list_devices))
        .route(
            "/api/v1/mobile/devices/{fingerprint}",
            delete(mobile::unpair_device),
        )
        // WebSocket for real-time events
        .route("/api/v1/ws", get(ws_handler))
        // Agents (Phase 4 read-only routes)
        .nest("/api/v1/agents", crate::server_agents::router())
        // Commander governance receiver
        .route(
            "/api/v1/governance/directive",
            post(governance::post_directive),
        )
        .route(
            "/api/v1/governance/audit/{fleet}",
            get(governance::get_audit),
        );

    // When a token is configured (non-loopback bind), require it on /api/*.
    let router = match auth_token {
        Some(token) => router.layer(middleware::from_fn_with_state(token, require_auth)),
        None => router,
    };

    router
        .layer(cors)
        .with_state(Arc::new(state))
        // Fallback: serve embedded web UI
        .fallback(get(serve_web_ui))
}

/// Bearer-token auth for `/api/*` routes. Applied only when the server is bound
/// beyond loopback (see [`run_server`]); `/api/v1/health` is exempt so liveness
/// probes work, and non-`/api/` paths (the static UI) are untouched.
async fn require_auth(State(token): State<Arc<str>>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let needs_token = path.starts_with("/api/") && path != "/api/v1/health";
    if needs_token {
        let presented = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "));
        if !presented.is_some_and(|p| ct_eq(p.as_bytes(), token.as_bytes())) {
            return (StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response();
        }
    }
    next.run(req).await
}

/// Constant-time byte comparison (avoids leaking the token via early-return
/// timing). Length is allowed to differ observably — that's not secret.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Start the API server on the given port.
/// If `open_url` is Some, opens the browser after binding.
pub async fn run_server(
    state: AppState,
    port: u16,
    open_url: Option<String>,
) -> anyhow::Result<()> {
    // Bind loopback by default. The API has no built-in auth, so exposing it
    // beyond loopback is an explicit opt-in (MUR_SERVER_HOST) that REQUIRES a
    // bearer token (MUR_SERVER_TOKEN) — otherwise any host on the network could
    // read and write every pattern/skill/agent (and delete them).
    let host = std::env::var("MUR_SERVER_HOST")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let token = std::env::var("MUR_SERVER_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());

    let is_loopback = matches!(host.trim(), "127.0.0.1" | "::1" | "[::1]" | "localhost");
    if !is_loopback && token.is_none() {
        anyhow::bail!(
            "refusing to bind {host}: the dashboard API has no built-in authentication, \
             so exposing it beyond loopback requires a token. Set MUR_SERVER_TOKEN (and send \
             `Authorization: Bearer <token>` on /api/* requests), or keep the default \
             127.0.0.1 bind."
        );
    }

    let app = build_router_with_auth(state, token.as_deref().map(Arc::from));
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("🚀 MUR server listening on http://{host}:{port}");
    eprintln!("   Dashboard: http://{host}:{port}");
    eprintln!("   API: http://{host}:{port}/api/v1/");
    if token.is_some() {
        eprintln!("   🔒 bearer-token auth enforced on /api/*");
    } else {
        eprintln!("   ⚠ loopback only, no auth — set MUR_SERVER_TOKEN before exposing remotely");
    }

    if let Some(url) = open_url {
        // Open browser after bind (server is ready)
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(&url).spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", &url])
                .spawn();
        }
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

// ─── System handlers (ws / ui / health / rates) ───────────────────

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.events_tx.subscribe();
    while let Ok(msg) = rx.recv().await {
        if socket.send(Message::Text(msg.into())).await.is_err() {
            break;
        }
    }
}

/// Broadcast an event to all connected WebSocket clients.
pub(super) fn notify(state: &AppState, event_type: &str, id: &str) {
    let msg =
        serde_json::json!({ "type": event_type, "id": id, "ts": chrono::Utc::now().to_rfc3339() });
    let _ = state.events_tx.send(msg.to_string());
}

async fn serve_web_ui(uri: axum::http::Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    // Try exact file first, then fallback to index.html (SPA)
    let file = WebAssets::get(path).or_else(|| WebAssets::get("index.html"));
    match file {
        Some(content) => {
            let mime = if path.ends_with(".js") {
                "application/javascript"
            } else if path.ends_with(".css") {
                "text/css"
            } else if path.ends_with(".svg") {
                "image/svg+xml"
            } else if path.ends_with(".png") {
                "image/png"
            } else if path.ends_with(".woff2") {
                "font/woff2"
            } else {
                "text/html"
            };
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime)],
                Body::from(content.data.to_vec()),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "source": "local",
    }))
}

/// `GET /api/v1/rates` — all current USD spot rates.
async fn get_rates() -> Result<impl IntoResponse, AppError> {
    let snapshot = fetch_usd_rates().await.map_err(AppError::Internal)?;
    Ok(Json(snapshot))
}

/// `GET /api/v1/rates/{currency}` — single USD spot rate, e.g. `/api/v1/rates/EUR`.
async fn get_rate(Path(currency): Path<String>) -> Result<impl IntoResponse, AppError> {
    let rate = fetch_usd_rate(&currency)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(rate))
}

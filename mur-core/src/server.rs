//! Local API server for MUR — `mur serve`.
//!
//! Exposes the pattern and workflow stores over HTTP so the web dashboard
//! (mur.run SPA or localhost dev) can read and write data.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Json, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_http::cors::{AllowHeaders, AllowMethods, CorsLayer};

use mur_common::knowledge::{KnowledgeBase, Maturity};

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
use mur_common::pattern::*;
use mur_common::workflow::Workflow;

use crate::context_api;
use crate::retrieve::scoring::{ScoredPattern, score_and_rank};
use crate::store::config::load_config;
use crate::store::embedding::{EmbeddingConfig, embed};
use crate::store::lancedb::VectorStore;
use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::YamlStore;

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
    /// Path to the LanceDB vector index (`~/.mur/index`).
    /// When present the context endpoint uses hybrid scoring.
    pub index_dir: PathBuf,
    pub config: ServerConfig,
    pub events_tx: broadcast::Sender<String>,
}

impl AppState {
    fn pattern_store(&self) -> Result<YamlStore, AppError> {
        YamlStore::new(self.patterns_dir.clone()).map_err(AppError::Internal)
    }

    fn workflow_store(&self) -> Result<WorkflowYamlStore, AppError> {
        WorkflowYamlStore::new(self.workflows_dir.clone()).map_err(AppError::Internal)
    }
}

// ─── Error type ────────────────────────────────────────────────────

#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    Readonly,
    BadRequest(String),
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
            AppError::Internal(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        let body = serde_json::json!({ "error": message });
        (status, Json(body)).into_response()
    }
}

// ─── Envelope ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct ApiResponse<T: Serialize> {
    data: T,
    meta: ApiMeta,
}

#[derive(Serialize)]
struct ApiMeta {
    source: &'static str,
    version: &'static str,
    pattern_count: usize,
}

fn wrap<T: Serialize>(data: T, pattern_count: usize) -> Json<ApiResponse<T>> {
    Json(ApiResponse {
        data,
        meta: ApiMeta {
            source: "local",
            version: env!("CARGO_PKG_VERSION"),
            pattern_count,
        },
    })
}

// ─── Router ────────────────────────────────────────────────────────

/// Build the axum router with all API endpoints.
pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:5173".parse().unwrap(),
            "https://mur.run".parse().unwrap(),
            "https://www.mur.run".parse().unwrap(),
            "https://mur-run.github.io".parse().unwrap(),
        ])
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any());

    Router::new()
        // Health
        .route("/api/v1/health", get(health))
        // Patterns CRUD
        .route("/api/v1/patterns", get(list_patterns))
        .route("/api/v1/patterns", post(create_pattern))
        .route("/api/v1/patterns/{id}", get(get_pattern))
        .route("/api/v1/patterns/{id}", put(update_pattern))
        .route("/api/v1/patterns/{id}", delete(delete_pattern))
        // Workflows CRUD
        .route("/api/v1/workflows", get(list_workflows))
        .route("/api/v1/workflows", post(create_workflow))
        .route("/api/v1/workflows/{id}", get(get_workflow))
        .route("/api/v1/workflows/{id}", put(update_workflow))
        .route("/api/v1/workflows/{id}", delete(delete_workflow))
        // Stats & metadata
        .route("/api/v1/stats", get(get_stats))
        .route("/api/v1/tags", get(get_tags))
        .route("/api/v1/links/{id}", get(get_links))
        // Search
        .route("/api/v1/search", post(search_patterns))
        // Context API (retrieve, ingest, feedback)
        .route("/api/v1/context", post(context_retrieve))
        .route("/api/v1/ingest", post(context_ingest))
        .route("/api/v1/feedback", post(context_feedback))
        // WebSocket for real-time events
        .route("/api/v1/ws", get(ws_handler))
        .layer(cors)
        .with_state(Arc::new(state))
        // Fallback: serve embedded web UI
        .fallback(get(serve_web_ui))
}

/// Start the API server on the given port.
/// If `open_url` is Some, opens the browser after binding.
pub async fn run_server(
    state: AppState,
    port: u16,
    open_url: Option<String>,
) -> anyhow::Result<()> {
    let app = build_router(state);
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("🚀 MUR server listening on http://localhost:{}", port);
    eprintln!("   Dashboard: http://localhost:{}", port);
    eprintln!("   API: http://localhost:{}/api/v1/", port);

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

    axum::serve(listener, app).await?;
    Ok(())
}

// ─── Handlers ──────────────────────────────────────────────────────

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
fn notify(state: &AppState, event_type: &str, id: &str) {
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

// ── Patterns ───────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct PatternFilter {
    pub tier: Option<String>,
    pub maturity: Option<String>,
    pub tag: Option<String>,
    pub status: Option<String>,
}

async fn list_patterns(
    State(state): State<Arc<AppState>>,
    Query(filter): Query<PatternFilter>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let mut patterns = store.list_all().map_err(AppError::Internal)?;

    // Apply filters
    if let Some(tier) = &filter.tier {
        let tier_lower = tier.to_lowercase();
        patterns.retain(|p| format!("{:?}", p.tier).to_lowercase() == tier_lower);
    }
    if let Some(maturity) = &filter.maturity {
        let mat_lower = maturity.to_lowercase();
        patterns.retain(|p| format!("{:?}", p.maturity).to_lowercase() == mat_lower);
    }
    if let Some(tag) = &filter.tag {
        let tag_lower = tag.to_lowercase();
        patterns.retain(|p| {
            p.tags
                .topics
                .iter()
                .chain(p.tags.languages.iter())
                .any(|t| t.to_lowercase() == tag_lower)
        });
    }
    if let Some(status) = &filter.status {
        let status_lower = status.to_lowercase();
        patterns.retain(|p| format!("{:?}", p.lifecycle.status).to_lowercase() == status_lower);
    }

    let count = patterns.len();
    Ok(wrap(patterns, count))
}

async fn get_pattern(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let pattern = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", id)))?;
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(pattern, count))
}

#[derive(Deserialize)]
pub struct CreatePatternRequest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub technical: Option<String>,
    #[serde(default)]
    pub principle: Option<String>,
    #[serde(default)]
    pub plain_content: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

async fn create_pattern(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreatePatternRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;
    if store.exists(&req.name) {
        return Err(AppError::BadRequest(format!(
            "Pattern '{}' already exists",
            req.name
        )));
    }

    let content = if let Some(tech) = req.technical {
        Content::DualLayer {
            technical: tech,
            principle: req.principle,
        }
    } else if let Some(plain) = req.plain_content {
        Content::Plain(plain)
    } else {
        return Err(AppError::BadRequest(
            "Must provide 'technical' or 'plain_content'".to_string(),
        ));
    };

    let tier = match req.tier.as_deref() {
        Some("project") => Tier::Project,
        Some("core") => Tier::Core,
        _ => Tier::Session,
    };

    let pattern = Pattern {
        base: KnowledgeBase {
            schema: SCHEMA_VERSION,
            name: req.name.clone(),
            description: req.description,
            content,
            tier,
            confidence: req.confidence.unwrap_or(0.7),
            tags: Tags {
                topics: req.tags.unwrap_or_default(),
                ..Tags::default()
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    };

    store.save(&pattern).map_err(AppError::Internal)?;
    notify(&state, "pattern:created", &req.name);
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok((StatusCode::CREATED, wrap(pattern, count)))
}

async fn update_pattern(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(updates): Json<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;
    let mut pattern = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", id)))?;

    // Apply partial updates
    if let Some(desc) = updates.get("description").and_then(|v| v.as_str()) {
        pattern.description = desc.to_string();
    }
    if let Some(tech) = updates.get("technical").and_then(|v| v.as_str()) {
        pattern.content = Content::DualLayer {
            technical: tech.to_string(),
            principle: updates
                .get("principle")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| match &pattern.content {
                    Content::DualLayer { principle, .. } => principle.clone(),
                    Content::Plain(_) => None,
                }),
        };
    }
    if let Some(conf) = updates.get("confidence").and_then(|v| v.as_f64()) {
        pattern.confidence = conf.clamp(0.0, 1.0);
    }
    if let Some(imp) = updates.get("importance").and_then(|v| v.as_f64()) {
        pattern.importance = imp.clamp(0.0, 1.0);
    }
    if let Some(tier_str) = updates.get("tier").and_then(|v| v.as_str()) {
        pattern.tier = match tier_str {
            "project" => Tier::Project,
            "core" => Tier::Core,
            _ => Tier::Session,
        };
    }
    if let Some(tags) = updates.get("tags").and_then(|v| v.as_array()) {
        pattern.tags.topics = tags
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }

    pattern.updated_at = chrono::Utc::now();
    store.save(&pattern).map_err(AppError::Internal)?;
    notify(&state, "pattern:updated", &id);
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(pattern, count))
}

async fn delete_pattern(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;
    let deleted = store
        .delete(&id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", id)))?;
    if !deleted {
        return Err(AppError::NotFound(format!("Pattern '{}' not found", id)));
    }
    notify(&state, "pattern:deleted", &id);
    Ok(StatusCode::NO_CONTENT)
}

// ── Workflows ──────────────────────────────────────────────────────

async fn list_workflows(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, AppError> {
    let wf_store = state.workflow_store()?;
    let workflows = wf_store.list_all().map_err(AppError::Internal)?;
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(workflows, count))
}

async fn get_workflow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.workflow_store()?;
    let workflow = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Workflow '{}' not found", id)))?;
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(workflow, count))
}

#[derive(Deserialize)]
pub struct CreateWorkflowRequest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

async fn create_workflow(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateWorkflowRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.workflow_store()?;
    if store.exists(&req.name) {
        return Err(AppError::BadRequest(format!(
            "Workflow '{}' already exists",
            req.name
        )));
    }

    let workflow = Workflow {
        base: KnowledgeBase {
            name: req.name.clone(),
            description: req.description,
            content: Content::Plain(req.content.unwrap_or_default()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        trigger: req.trigger.unwrap_or_default(),
        tools: req.tools.unwrap_or_default(),
        steps: vec![],
        variables: vec![],
        source_sessions: vec![],
        published_version: 0,
        permission: Default::default(),
    };

    store.save(&workflow).map_err(AppError::Internal)?;
    notify(&state, "workflow:created", &req.name);
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok((StatusCode::CREATED, wrap(workflow, count)))
}

async fn update_workflow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(updates): Json<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.workflow_store()?;
    let mut workflow = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Workflow '{}' not found", id)))?;

    if let Some(desc) = updates.get("description").and_then(|v| v.as_str()) {
        workflow.description = desc.to_string();
    }
    if let Some(trigger) = updates.get("trigger").and_then(|v| v.as_str()) {
        workflow.trigger = trigger.to_string();
    }
    if let Some(tools) = updates.get("tools").and_then(|v| v.as_array()) {
        workflow.tools = tools
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }

    workflow.updated_at = chrono::Utc::now();
    store.save(&workflow).map_err(AppError::Internal)?;
    notify(&state, "workflow:updated", &id);
    let p_store = state.pattern_store()?;
    let count = p_store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(workflow, count))
}

async fn delete_workflow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.workflow_store()?;
    let deleted = store
        .delete(&id)
        .map_err(|_| AppError::NotFound(format!("Workflow '{}' not found", id)))?;
    if !deleted {
        return Err(AppError::NotFound(format!("Workflow '{}' not found", id)));
    }
    notify(&state, "workflow:deleted", &id);
    Ok(StatusCode::NO_CONTENT)
}

// ── Stats & metadata ───────────────────────────────────────────────

#[derive(Serialize)]
struct StatsResponse {
    total_patterns: usize,
    total_workflows: usize,
    by_tier: TierCounts,
    by_maturity: MaturityCounts,
    by_status: StatusCounts,
    total_injections: u64,
    avg_confidence: f64,
    avg_importance: f64,
}

#[derive(Serialize, Default)]
struct TierCounts {
    session: usize,
    project: usize,
    core: usize,
}

#[derive(Serialize, Default)]
struct MaturityCounts {
    draft: usize,
    emerging: usize,
    stable: usize,
    canonical: usize,
}

#[derive(Serialize, Default)]
struct StatusCounts {
    active: usize,
    deprecated: usize,
    archived: usize,
}

async fn get_stats(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let patterns = store.list_all().map_err(AppError::Internal)?;
    let wf_store = state.workflow_store()?;
    let workflows = wf_store.list_all().map_err(AppError::Internal)?;

    let mut tier = TierCounts::default();
    let mut maturity = MaturityCounts::default();
    let mut status = StatusCounts::default();
    let mut total_injections = 0u64;
    let mut total_confidence = 0.0f64;
    let mut total_importance = 0.0f64;

    for p in &patterns {
        match p.tier {
            Tier::Session => tier.session += 1,
            Tier::Project => tier.project += 1,
            Tier::Core => tier.core += 1,
        }
        match p.maturity {
            Maturity::Draft => maturity.draft += 1,
            Maturity::Emerging => maturity.emerging += 1,
            Maturity::Stable => maturity.stable += 1,
            Maturity::Canonical => maturity.canonical += 1,
        }
        match p.lifecycle.status {
            LifecycleStatus::Active => status.active += 1,
            LifecycleStatus::Deprecated => status.deprecated += 1,
            LifecycleStatus::Archived => status.archived += 1,
        }
        total_injections += p.evidence.injection_count;
        total_confidence += p.confidence;
        total_importance += p.importance;
    }

    let n = patterns.len().max(1) as f64;
    let stats = StatsResponse {
        total_patterns: patterns.len(),
        total_workflows: workflows.len(),
        by_tier: tier,
        by_maturity: maturity,
        by_status: status,
        total_injections,
        avg_confidence: total_confidence / n,
        avg_importance: total_importance / n,
    };

    Ok(wrap(stats, patterns.len()))
}

async fn get_tags(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let patterns = store.list_all().map_err(AppError::Internal)?;

    let mut tag_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for p in &patterns {
        for t in p.tags.topics.iter().chain(p.tags.languages.iter()) {
            *tag_counts.entry(t.clone()).or_default() += 1;
        }
    }

    let tags: Vec<TagInfo> = tag_counts
        .into_iter()
        .map(|(name, count)| TagInfo { name, count })
        .collect();

    Ok(wrap(tags, patterns.len()))
}

#[derive(Serialize)]
struct TagInfo {
    name: String,
    count: usize,
}

async fn get_links(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let pattern = store
        .get(&id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", id)))?;
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(pattern.links.clone(), count))
}

// ── Search ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Serialize)]
struct SearchResult {
    name: String,
    description: String,
    score: f64,
    relevance: f64,
    tier: String,
    maturity: String,
    confidence: f64,
}

async fn search_patterns(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;
    let patterns = store.list_all().map_err(AppError::Internal)?;
    let count = patterns.len();

    let scored: Vec<ScoredPattern> = score_and_rank(&req.query, patterns);

    let results: Vec<SearchResult> = scored
        .into_iter()
        .take(req.limit)
        .map(|sp| SearchResult {
            name: sp.pattern.name.clone(),
            description: sp.pattern.description.clone(),
            score: sp.score,
            relevance: sp.relevance,
            tier: format!("{:?}", sp.pattern.tier).to_lowercase(),
            maturity: format!("{:?}", sp.pattern.maturity).to_lowercase(),
            confidence: sp.pattern.confidence,
        })
        .collect();

    Ok(wrap(results, count))
}

// ─── Context API Handlers ─────────────────────────────────────────

async fn context_retrieve(
    State(state): State<Arc<AppState>>,
    Json(req): Json<context_api::ContextRequest>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.pattern_store()?;

    // Attempt hybrid (vector + keyword) scoring when the LanceDB index exists.
    // Falls back to keyword-only when the index is absent or embedding fails.
    let vector_scores = if state.index_dir.exists() {
        match load_config() {
            Ok(cfg) => {
                let emb_cfg = EmbeddingConfig::from_config(&cfg);
                match embed(&req.query, &emb_cfg).await {
                    Ok(query_embedding) => {
                        match VectorStore::open(&state.index_dir, cfg.embedding.dimensions as i32)
                            .await
                        {
                            Ok(vs) => {
                                vs.search(&query_embedding, 20, None)
                                    .await
                                    .ok()
                                    .map(|results| {
                                        results
                                            .into_iter()
                                            .map(|r| (r.name, r.similarity as f64))
                                            .collect::<std::collections::HashMap<_, _>>()
                                    })
                            }
                            Err(_) => None,
                        }
                    }
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    } else {
        None
    };

    let resp =
        context_api::retrieve(&req, &store, vector_scores.as_ref()).map_err(AppError::Internal)?;
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(resp, count))
}

async fn context_ingest(
    State(state): State<Arc<AppState>>,
    Json(req): Json<context_api::IngestRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;
    let resp = context_api::ingest(&req, &store).map_err(AppError::Internal)?;
    notify(&state, "pattern:ingested", &resp.pattern_id);
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok((StatusCode::CREATED, wrap(resp, count)))
}

async fn context_feedback(
    State(state): State<Arc<AppState>>,
    Json(req): Json<context_api::FeedbackRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }
    let store = state.pattern_store()?;

    // Check pattern exists first for a clear 404
    store
        .get(&req.pattern_id)
        .map_err(|_| AppError::NotFound(format!("Pattern '{}' not found", req.pattern_id)))?;

    context_api::submit_feedback(&req, &store).map_err(AppError::Internal)?;
    notify(&state, "pattern:feedback", &req.pattern_id);
    let count = store.list_names().map(|n| n.len()).unwrap_or(0);
    Ok(wrap(serde_json::json!({"ok": true}), count))
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state(tmp: &tempfile::TempDir) -> AppState {
        let patterns_dir = tmp.path().join("patterns");
        let workflows_dir = tmp.path().join("workflows");
        let index_dir = tmp.path().join("index"); // non-existent → keyword-only fallback
        std::fs::create_dir_all(&patterns_dir).unwrap();
        std::fs::create_dir_all(&workflows_dir).unwrap();
        let (events_tx, _) = broadcast::channel(64);
        AppState {
            patterns_dir,
            workflows_dir,
            index_dir,
            config: ServerConfig { readonly: false },
            events_tx,
        }
    }

    fn test_state_readonly(tmp: &tempfile::TempDir) -> AppState {
        let mut state = test_state(tmp);
        state.config.readonly = true;
        state
    }

    fn make_test_pattern(name: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: name.to_string(),
                description: format!("Test: {}", name),
                content: Content::Plain(format!("Content for {}", name)),
                tier: Tier::Session,
                confidence: 0.8,
                tags: Tags {
                    topics: vec!["rust".into(), "testing".into()],
                    ..Tags::default()
                },
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        }
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_health() {
        let tmp = tempfile::tempdir().unwrap();
        let app = build_router(test_state(&tmp));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["source"], "local");
    }

    #[tokio::test]
    async fn test_list_patterns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let app = build_router(test_state(&tmp));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/patterns")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["data"], serde_json::json!([]));
        assert_eq!(json["meta"]["pattern_count"], 0);
    }

    #[tokio::test]
    async fn test_create_and_get_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);
        let app = build_router(state.clone());

        // Create
        let body = serde_json::json!({
            "name": "test-pattern",
            "description": "A test",
            "technical": "Use this for testing",
            "tags": ["rust", "test"]
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/patterns")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);

        // Get
        let app2 = build_router(state);
        let resp = app2
            .oneshot(
                Request::builder()
                    .uri("/api/v1/patterns/test-pattern")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["data"]["name"], "test-pattern");
    }

    #[tokio::test]
    async fn test_update_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);

        // Seed a pattern
        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        store.save(&make_test_pattern("update-me")).unwrap();

        let app = build_router(state);

        let body = serde_json::json!({
            "description": "Updated description",
            "confidence": 0.95
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/patterns/update-me")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["data"]["description"], "Updated description");
        assert_eq!(json["data"]["confidence"], 0.95);
    }

    #[tokio::test]
    async fn test_delete_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);

        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        store.save(&make_test_pattern("to-delete")).unwrap();

        let app = build_router(state.clone());

        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/patterns/to-delete")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Verify it's gone
        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        assert!(!store.exists("to-delete"));
    }

    #[tokio::test]
    async fn test_pattern_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let app = build_router(test_state(&tmp));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/patterns/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_readonly_rejects_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let app = build_router(test_state_readonly(&tmp));

        let body = serde_json::json!({
            "name": "blocked",
            "description": "Should fail",
            "technical": "content"
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/patterns")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_get_stats() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);

        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        store.save(&make_test_pattern("stat-1")).unwrap();
        store.save(&make_test_pattern("stat-2")).unwrap();

        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["data"]["total_patterns"], 2);
    }

    #[tokio::test]
    async fn test_get_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);

        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        store.save(&make_test_pattern("tagged-1")).unwrap();

        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let tags = json["data"].as_array().unwrap();
        assert!(!tags.is_empty());
    }

    #[tokio::test]
    async fn test_search() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);

        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        let mut p = make_test_pattern("rust-error-handling");
        p.description = "Use thiserror for library errors".to_string();
        p.content = Content::Plain("rust error handling with anyhow and thiserror".to_string());
        p.tags.topics = vec!["rust".into(), "error".into()];
        store.save(&p).unwrap();

        let app = build_router(state);

        let body = serde_json::json!({ "query": "rust error" });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/search")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let results = json["data"].as_array().unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0]["name"], "rust-error-handling");
    }

    #[tokio::test]
    async fn test_filter_by_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);

        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        let mut p1 = make_test_pattern("session-pat");
        p1.tier = Tier::Session;
        store.save(&p1).unwrap();

        let mut p2 = make_test_pattern("core-pat");
        p2.tier = Tier::Core;
        store.save(&p2).unwrap();

        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/patterns?tier=core")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["name"], "core-pat");
    }

    #[tokio::test]
    async fn test_workflow_crud() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);

        // Create
        let app = build_router(state.clone());
        let body = serde_json::json!({
            "name": "deploy-workflow",
            "description": "Deploy to production",
            "trigger": "deploy",
            "tools": ["bash", "docker"]
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/workflows")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // List
        let app2 = build_router(state.clone());
        let resp = app2
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["data"].as_array().unwrap().len(), 1);

        // Get
        let app3 = build_router(state.clone());
        let resp = app3
            .oneshot(
                Request::builder()
                    .uri("/api/v1/workflows/deploy-workflow")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["data"]["name"], "deploy-workflow");

        // Delete
        let app4 = build_router(state.clone());
        let resp = app4
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/v1/workflows/deploy-workflow")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_cors_headers() {
        let tmp = tempfile::tempdir().unwrap();
        let app = build_router(test_state(&tmp));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header("Origin", "http://localhost:5173")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let acl = resp
            .headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap().to_string());
        assert_eq!(acl.as_deref(), Some("http://localhost:5173"));
    }

    #[tokio::test]
    async fn test_duplicate_pattern_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);

        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        store.save(&make_test_pattern("existing")).unwrap();

        let app = build_router(state);

        let body = serde_json::json!({
            "name": "existing",
            "description": "duplicate",
            "technical": "content"
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/patterns")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_get_links() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);

        let mut p = make_test_pattern("linked-pattern");
        p.links.related = vec!["other-pattern".into()];
        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        store.save(&p).unwrap();

        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/links/linked-pattern")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let related = json["data"]["related"].as_array().unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0], "other-pattern");
    }

    // ─── Context API endpoint tests ────────────────────────────

    #[tokio::test]
    async fn test_context_retrieve_200() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);
        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        store.save(&make_test_pattern("ctx-pattern")).unwrap();

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/context")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query":"rust testing","source":"test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(json["data"]["patterns"].is_array());
        assert!(json["data"]["formatted"].is_string());
    }

    #[tokio::test]
    async fn test_context_ingest_201() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"content":"Run tests before deploy","category":"procedure","source":"test","name":"test-deploy-rule"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let json = body_json(resp).await;
        assert_eq!(json["data"]["action"], "created");
    }

    #[tokio::test]
    async fn test_context_ingest_readonly_403() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state_readonly(&tmp);
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"content":"test","category":"fact","source":"x"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_context_feedback_200() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);
        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        store.save(&make_test_pattern("fb-pattern")).unwrap();

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/feedback")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"pattern_id":"fb-pattern","signal":"success","source":"test"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_context_feedback_404() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(&tmp);
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/feedback")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"pattern_id":"nonexistent","signal":"success","source":"test"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

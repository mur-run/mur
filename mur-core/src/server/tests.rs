//! Server integration tests — exercise the router end-to-end via tower.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tokio::sync::broadcast;
use tower::ServiceExt;

use super::*;

fn test_state(tmp: &tempfile::TempDir) -> AppState {
    let patterns_dir = tmp.path().join("patterns");
    let workflows_dir = tmp.path().join("workflows");
    let pipelines_dir = tmp.path().join("pipelines");
    let agents_dir = tmp.path().join("agents");
    let index_dir = tmp.path().join("index"); // non-existent → keyword-only fallback
    std::fs::create_dir_all(&patterns_dir).unwrap();
    std::fs::create_dir_all(&workflows_dir).unwrap();
    std::fs::create_dir_all(&pipelines_dir).unwrap();
    std::fs::create_dir_all(&agents_dir).unwrap();
    let (events_tx, _) = broadcast::channel(64);
    AppState {
        patterns_dir,
        workflows_dir,
        pipelines_dir,
        agents_dir,
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

/// Re-exported for use by sibling module tests (e.g. signals::tests).
pub(super) fn test_state_for_signals(tmp: &tempfile::TempDir) -> AppState {
    test_state(tmp)
}

fn skill_body(name: &str) -> serde_json::Value {
    serde_json::json!({
        "manifest": {
            "name": name,
            "version": "1.0.0",
            "publisher": "human:t",
            "description": "A test skill",
            "category": "context",
            "content": { "abstract": "a test skill body" }
        }
    })
}

/// Write a minimal installed skill under `<mur_home>/skills/<name>/` with
/// the given tags and `requires`, bypassing the HTTP create route.
fn install_test_skill(state: &AppState, name: &str, tags: &[&str], requires: &[&str]) {
    let dir = state.skills_dir().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut yaml = format!(
        "name: {name}\nversion: 1.0.0\npublisher: human:t\ndescription: Test {name}\ncategory: context\n"
    );
    if !tags.is_empty() {
        yaml.push_str("tags:\n");
        for t in tags {
            yaml.push_str(&format!("  - {t}\n"));
        }
    }
    if !requires.is_empty() {
        yaml.push_str("requires:\n");
        for r in requires {
            yaml.push_str(&format!("  - name: {r}\n"));
        }
    }
    yaml.push_str("content:\n  abstract: a test skill body\n");
    std::fs::write(dir.join("skill.yaml"), yaml).unwrap();
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
async fn pattern_routes_are_gone() {
    // Tier B4 of the Pattern -> Skill retirement (#2264): the dashboard reads
    // `/api/v1/skills`; the legacy pattern CRUD must no longer be mounted.
    // Unmatched GETs fall through to the SPA fallback (HTML, never JSON);
    // unmatched mutations have no handler at all.
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));

    for uri in ["/api/v1/patterns", "/api/v1/patterns/x"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let ct = resp
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        assert!(
            !ct.contains("json"),
            "GET {uri} still served by an API handler ({ct})"
        );
    }

    for (method, uri) in [
        ("POST", "/api/v1/patterns"),
        ("PUT", "/api/v1/patterns/x"),
        ("DELETE", "/api/v1/patterns/x"),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {uri}"
        );
    }
}

#[tokio::test]
async fn skill_routes_reject_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));

    // DELETE with a traversal name must NOT remove_dir_all the parent (mur home):
    // skills_dir is tmp/skills, so `..` would target tmp itself.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/skills/..")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK);
    assert!(
        tmp.path().join("patterns").exists(),
        "DELETE /skills/.. must not delete the mur home"
    );

    // GET with an encoded traversal (`..%2F..%2Fpatterns` → `../../patterns`)
    // must be rejected, not read outside skills_dir.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/skills/..%2F..%2Fpatterns")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn auth_token_enforced_on_api_when_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let token: std::sync::Arc<str> = std::sync::Arc::from("s3cret-token");
    let app = build_router_with_auth(test_state(&tmp), Some(token));

    let api_req = |auth: Option<&str>| {
        let mut b = Request::builder().uri("/api/v1/skills");
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        b.body(Body::empty()).unwrap()
    };

    // No token → 401.
    let resp = app.clone().oneshot(api_req(None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Wrong token → 401.
    let resp = app
        .clone()
        .oneshot(api_req(Some("Bearer nope")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Correct token → 200.
    let resp = app
        .clone()
        .oneshot(api_req(Some("Bearer s3cret-token")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // Health is exempt so liveness probes work without a token.
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
}

#[tokio::test]
async fn test_readonly_rejects_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state_readonly(&tmp));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/skills")
                .header("Content-Type", "application/json")
                .body(Body::from(skill_body("blocked").to_string()))
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

    install_test_skill(&state, "stat-1", &[], &[]);
    install_test_skill(&state, "stat-2", &[], &[]);

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
    assert_eq!(json["data"]["by_category"]["context"], 2);
    assert_eq!(json["data"]["by_lifecycle"]["draft"], 2);
    assert_eq!(json["meta"]["pattern_count"], 2);
}

#[tokio::test]
async fn test_get_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(&tmp);

    install_test_skill(&state, "tagged-1", &["rust", "testing"], &[]);

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
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0]["name"], "rust");
    assert_eq!(tags[0]["count"], 1);
}

#[tokio::test]
async fn test_search() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(&tmp);

    // Seed a skill in the skills_dir (search now reads skills, not patterns)
    let skill_dir = state.skills_dir().join("rust-error-handling");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("skill.yaml"),
        "name: rust-error-handling\nversion: 1.0.0\npublisher: human:t\n\
         description: Use thiserror for library errors\ncategory: context\n\
         content:\n  abstract: rust error handling with anyhow and thiserror\n\
         tags:\n  - rust\n  - error\n",
    )
    .unwrap();

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
async fn test_duplicate_skill_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));

    let post = || {
        Request::builder()
            .method("POST")
            .uri("/api/v1/skills")
            .header("Content-Type", "application/json")
            .body(Body::from(skill_body("existing").to_string()))
            .unwrap()
    };

    let resp = app.clone().oneshot(post()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app.oneshot(post()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_get_links() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(&tmp);

    install_test_skill(&state, "linked-skill", &[], &["other-skill"]);

    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/links/linked-skill")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let related = json["data"]["related"].as_array().unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0], "other-skill");
}

// ─── Context API endpoint tests ────────────────────────────

#[tokio::test]
async fn test_context_retrieve_200() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(&tmp);
    let skill = tmp.path().join("skills").join("ctx-skill");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("skill.yaml"),
        "name: ctx-skill\nversion: 1.0.0\npublisher: human:test\n\
         description: rust testing with nextest\ncategory: context\npriority: normal\n\
         tags: [rust, testing]\ncontent:\n  abstract: rust testing with nextest\n  context: x\n",
    )
    .unwrap();

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
    assert_eq!(json["data"]["patterns"][0]["name"], "ctx-skill");
    assert!(json["data"]["formatted"].is_string());
}

/// `/api/v1/ingest` and `/api/v1/feedback` wrote to the retired Pattern
/// store; they must be unmounted. As in `pattern_routes_are_gone`, an
/// unmatched mutation has no handler and the GET-only SPA fallback answers
/// 405 — a 2xx/4xx JSON body would mean a handler is still wired.
#[tokio::test]
async fn ingest_and_feedback_routes_are_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));
    for uri in ["/api/v1/ingest", "/api/v1/feedback"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{uri}");
    }
}

// ── Exchange rate handler tests ────────────────────────────────────────────

/// Live integration test — requires network. Run with `cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn test_get_rates_live() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/rates")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["base"], "USD");
    assert!(json["rates"].is_object());
    assert!(json["rates"]["EUR"].as_f64().unwrap_or(0.0) > 0.0);
}

/// Live integration test — requires network. Run with `cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn test_get_rate_single_currency_live() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/rates/EUR")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["base"], "USD");
    assert_eq!(json["target"], "EUR");
    assert!(json["rate"].as_f64().unwrap_or(0.0) > 0.0);
}

/// Live integration test — requires network. Run with `cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn test_get_rate_invalid_currency_returns_500_live() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/rates/NOTREAL")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn test_agents_router_mounted_returns_404_for_unknown_subpath() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));

    // /api/v1/agents/__nope__ should resolve into the agents subrouter
    // (and fall through to a 404), proving the nested router is mounted.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/agents/__nope__/__nope__")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// `GET /api/v1/schedules` is served, not swallowed by the SPA fallback.
///
/// The status code alone cannot show this: an unmatched path returns
/// `200 text/html`, which is how a first pass over this server concluded that
/// every endpoint existed. Content type is the signal.
#[tokio::test]
async fn test_schedules_serves_json() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/schedules")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ctype.starts_with("application/json"),
        "schedules answered {ctype} — the SPA fallback, not the route"
    );

    // Empty MUR home → no schedules, but a well-formed envelope.
    let json = body_json(resp).await;
    assert!(
        json["data"].is_array(),
        "expected a {{data, meta}} envelope"
    );
}

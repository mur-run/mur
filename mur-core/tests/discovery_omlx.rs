//! Wiremock-backed integration tests for OMlxDiscovery.

use mur_core::discovery::{Discovery, ModelKind, omlx::OMlxDiscovery};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── 4.1 tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_models_returns_unknown_kind_pre_probe() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "mlx-community/Qwen3-Embedding-0.6B-8bit"},
                {"id": "mlx-community/Qwen3.5-4B-4bit"}
            ]
        })))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();
    assert_eq!(models.len(), 2);
    // /v1/models has no type discriminator — kind is Unknown until probed.
    assert!(models.iter().all(|m| m.kind == ModelKind::Unknown));
    // Family is inferred from the id substring.
    assert_eq!(
        models
            .iter()
            .find(|m| m.id.contains("Qwen3-Embedding"))
            .unwrap()
            .family
            .as_deref(),
        Some("qwen3")
    );
}

// ── 4.2 tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn probe_embedding_happy_path() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"embedding": vec![0.0f32; 1024]}]
        })))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let p = d
        .probe_embedding("mlx-community/Qwen3-Embedding-0.6B-8bit")
        .await
        .unwrap();
    assert_eq!(p.dims, 1024);
}

#[tokio::test]
async fn probe_embedding_4xx_errors() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("model does not support embeddings"),
        )
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let r = d
        .probe_embedding("mlx-community/Qwen3.5-4B-4bit")
        .await;
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("400"));
}

#[tokio::test]
async fn probe_embedding_empty_array_errors() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    assert!(d.probe_embedding("foo").await.is_err());
}

/// Verify that `list_models` infers the correct family from each model id.
///
/// `family_from_id` is private; we test it indirectly by inspecting the
/// `family` field on the returned `DiscoveredModel` values.
#[tokio::test]
async fn list_models_infers_family_from_id() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "mlx-community/bge-m3"},
                {"id": "lightonai/modernbert-embed-large"},
                {"id": "mlx-community/Qwen3-Embedding-0.6B-8bit"},
                {"id": "nomic-ai/nomic-embed-text-v1.5"},
                {"id": "jinaai/jina-embeddings-v3"},
                {"id": "unknown/foo"}
            ]
        })))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();

    let family_of = |id: &str| {
        models
            .iter()
            .find(|m| m.id == id)
            .unwrap()
            .family
            .as_deref()
            .map(str::to_string)
    };

    assert_eq!(family_of("mlx-community/bge-m3"), Some("bge".into()));
    assert_eq!(
        family_of("lightonai/modernbert-embed-large"),
        Some("modernbert".into())
    );
    assert_eq!(
        family_of("mlx-community/Qwen3-Embedding-0.6B-8bit"),
        Some("qwen3".into())
    );
    assert_eq!(
        family_of("nomic-ai/nomic-embed-text-v1.5"),
        Some("nomic-bert".into())
    );
    assert_eq!(
        family_of("jinaai/jina-embeddings-v3"),
        Some("jina-bert".into())
    );
    assert_eq!(family_of("unknown/foo"), None);
}

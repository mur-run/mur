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

/// Regression: oMLX has shipped multiple `/v1/models` response shapes
/// across versions. The parser must accept the OpenAI envelope, the
/// Ollama-style `{models:[]}`, and a bare array. This test caught a real
/// failure where a user's oMLX returned a non-`data` shape and the
/// parser bailed with "missing field `data`".
#[tokio::test]
async fn list_models_accepts_ollama_style_models_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                {"id": "mlx-community/Qwen3-Embedding-0.6B-8bit"}
            ]
        })))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "mlx-community/Qwen3-Embedding-0.6B-8bit");
}

#[tokio::test]
async fn list_models_accepts_bare_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "foo"},
            {"id": "bar"}
        ])))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();
    assert_eq!(models.len(), 2);
}

/// Regression: real-world oMLX returns 401 on `/v1/models` when an API key
/// isn't sent. `OMlxDiscovery::with_api_key` must include
/// `Authorization: Bearer <key>` on requests, and the 401 path must surface
/// a helpful "set OMLX_API_KEY" message.
#[tokio::test]
async fn list_models_sends_authorization_header() {
    use wiremock::matchers::header;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("Authorization", "Bearer secret-key-david"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "mlx-community/Qwen3-Embedding-0.6B-8bit"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let d = OMlxDiscovery::with_api_key(server.uri(), "secret-key-david");
    let models = d.list_models().await.unwrap();
    assert_eq!(models.len(), 1);
}

#[tokio::test]
async fn list_models_401_surfaces_omlx_api_key_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {
                "message": "API key required",
                "type": "authentication_error",
                "param": null,
                "code": null
            }
        })))
        .mount(&server)
        .await;

    let d = OMlxDiscovery::new(server.uri()); // no api key
    let r = d.list_models().await;
    assert!(r.is_err());
    let err_str = format!("{:#}", r.unwrap_err());
    assert!(
        err_str.contains("OMLX_API_KEY"),
        "401 error must hint at OMLX_API_KEY env var; got: {}",
        err_str
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

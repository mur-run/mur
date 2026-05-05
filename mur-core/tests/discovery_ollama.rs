//! Wiremock-backed integration tests for OllamaDiscovery.

use mur_core::discovery::{Discovery, ModelKind, ollama::OllamaDiscovery};
use serde_json::json;
use wiremock::matchers::{body_json_string, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tags_response_with(models: Vec<(&str, &str, u64)>) -> serde_json::Value {
    json!({
        "models": models.iter().map(|(name, family, size)| json!({
            "name": name,
            "size": size,
            "details": { "family": family }
        })).collect::<Vec<_>>()
    })
}

#[tokio::test]
async fn list_models_marks_capabilities_embedding_as_embedding_kind() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tags_response_with(vec![
            ("qwen3-embedding:0.6b", "bert", 700_000_000),
            ("qwen3.5:4b", "qwen3", 4_000_000_000),
        ])))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/show"))
        .and(body_json_string(r#"{"name":"qwen3-embedding:0.6b"}"#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities": ["embedding"]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/show"))
        .and(body_json_string(r#"{"name":"qwen3.5:4b"}"#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities": ["completion"]
        })))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();

    assert_eq!(models.len(), 2);
    let emb = models.iter().find(|m| m.id == "qwen3-embedding:0.6b").unwrap();
    assert_eq!(emb.kind, ModelKind::Embedding);
    assert_eq!(emb.family.as_deref(), Some("bert"));

    let llm = models.iter().find(|m| m.id == "qwen3.5:4b").unwrap();
    assert_eq!(llm.kind, ModelKind::Llm);
}

#[tokio::test]
async fn list_models_falls_back_to_family_when_capabilities_absent() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tags_response_with(vec![
            ("nomic-embed-text:latest", "nomic-bert", 300_000_000),
        ])))
        .mount(&server)
        .await;

    // /api/show returns no capabilities field → fallback path
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();
    assert_eq!(models[0].kind, ModelKind::Embedding);
}

#[tokio::test]
async fn list_models_marks_unreachable_show_as_unknown() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tags_response_with(vec![
            ("foo:bar", "weird-family", 100),
        ])))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let models = d.list_models().await.unwrap();
    assert_eq!(models[0].kind, ModelKind::Unknown);
}

#[tokio::test]
async fn probe_embedding_returns_dims_and_latency() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/embed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embeddings": [vec![0.0f32; 1024]]
        })))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let probe = d.probe_embedding("qwen3-embedding:0.6b").await.unwrap();
    assert_eq!(probe.dims, 1024);
}

#[tokio::test]
async fn probe_embedding_errors_on_4xx() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/embed"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let d = OllamaDiscovery::new(server.uri());
    let r = d.probe_embedding("missing").await;
    assert!(r.is_err());
}

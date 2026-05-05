use mur_core::discovery::{Backend, ModelKind};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn run_all_merges_ollama_and_omlx() {
    let ollama_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{
                "name": "qwen3-embedding:0.6b",
                "size": 700_000_000u64,
                "details": {"family": "bert"}
            }]
        })))
        .mount(&ollama_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities": ["embedding"]
        })))
        .mount(&ollama_server)
        .await;

    let omlx_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "mlx-community/Qwen3-Embedding-0.6B-8bit"}]
        })))
        .mount(&omlx_server)
        .await;

    let merged =
        mur_core::discovery::run_all_for_test(Some(ollama_server.uri()), Some(omlx_server.uri()))
            .await
            .unwrap();

    assert_eq!(merged.len(), 2);
    let backends: Vec<Backend> = merged.iter().map(|m| m.backend).collect();
    assert!(backends.contains(&Backend::Ollama));
    assert!(backends.contains(&Backend::OMlx));
    let ollama_model = merged
        .iter()
        .find(|m| m.backend == Backend::Ollama)
        .unwrap();
    assert_eq!(ollama_model.kind, ModelKind::Embedding);
    let omlx_model = merged.iter().find(|m| m.backend == Backend::OMlx).unwrap();
    assert_eq!(omlx_model.kind, ModelKind::Unknown); // not yet probed
}

#[tokio::test]
async fn run_all_skips_failing_backends() {
    let omlx_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&omlx_server)
        .await;

    // Ollama unreachable URL → discovery returns empty for that backend, no error
    let merged = mur_core::discovery::run_all_for_test(
        Some("http://127.0.0.1:1".into()),
        Some(omlx_server.uri()),
    )
    .await
    .unwrap();
    assert_eq!(merged.len(), 0);
}

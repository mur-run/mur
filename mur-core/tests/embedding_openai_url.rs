//! Integration test: embed_openai POSTs to the base_url from EmbeddingConfig,
//! not the hardcoded api.openai.com URL.

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn embed_openai_posts_to_custom_base_url() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"embedding": vec![0.1f32; 1024]}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = mur_common::config::Config::default();
    cfg.embedding.provider = "omlx".into();
    cfg.embedding.openai_url = Some(server.uri()); // mock acts as oMLX server
    cfg.embedding.model = "mlx-community/Qwen3-Embedding-0.6B-8bit".into();
    cfg.embedding.api_key_env = Some("OMLX_API_KEY".into());

    // SAFETY: each .rs file under mur-core/tests/ compiles as its own
    // integration-test binary, so this test is the only writer of
    // OMLX_API_KEY in this binary's process. If you add another test to
    // this file that also mutates OMLX_API_KEY, this assumption breaks
    // and the env access becomes a real data race.
    unsafe {
        std::env::set_var("OMLX_API_KEY", "local");
    }

    let ec = mur_core::store::embedding::EmbeddingConfig::from_config(&cfg);
    let v = mur_core::store::embedding::embed("hello", &ec).await.unwrap();
    assert_eq!(v.len(), 1024);
}

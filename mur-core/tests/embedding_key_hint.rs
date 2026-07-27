//! Integration test: when an embedding API key is *configured* but cannot be
//! resolved, the reason must travel all the way to the HTTP error.
//!
//! Regression guard for the unexplained `401 API key required`: the key
//! resolution chain (`api_key_ref` → `api_key_env` → `OPENAI_API_KEY`) used to
//! collapse to an empty string, send `Authorization: Bearer `, and surface a
//! bare 401 with no hint at which link broke. A background agent process that
//! cannot reach the OS keychain hits exactly that path, and the log line the
//! resolver writes is drained at `debug` by its supervisor — so the error
//! itself has to carry the reason.

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn unresolved_key_reason_reaches_the_http_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {"message": "API key required", "type": "authentication_error"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = mur_common::config::Config::default();
    cfg.embedding.provider = "omlx".into();
    cfg.embedding.openai_url = Some(server.uri());
    cfg.embedding.api_key_ref = Some("env:MUR_TEST_EMB_HINT_ABSENT".into());
    cfg.embedding.api_key_env = Some("MUR_TEST_EMB_HINT_ABSENT".into());

    // SAFETY: each .rs file under mur-core/tests/ compiles as its own
    // integration-test binary, so this test is the only writer of these vars
    // in this binary's process. `OPENAI_API_KEY` is blanked rather than
    // removed so the assertion holds whatever the ambient environment carries.
    unsafe {
        std::env::remove_var("MUR_TEST_EMB_HINT_ABSENT");
        std::env::set_var("OPENAI_API_KEY", "");
    }

    let ec = mur_core::store::embedding::EmbeddingConfig::from_config(&cfg);
    let err = mur_core::store::embedding::embed("hello", &ec)
        .await
        .expect_err("a 401 must not be swallowed")
        .to_string();

    assert!(err.contains("401"), "error should keep the status: {err}");
    assert!(
        err.contains("env:MUR_TEST_EMB_HINT_ABSENT"),
        "error should name the ref that failed to resolve: {err}"
    );
    assert!(
        err.contains("keychain"),
        "error should point at the likely cause: {err}"
    );
}
